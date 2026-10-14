// SPDX-License-Identifier: GPL-3.0-or-later
//! Test helpers for code that sets owner-only ACLs: read a handle's DACL back as the OS reports
//! it, open files from a thread impersonating a restricted copy of this process's token, and
//! make a real second local user ([`SecondUser`]) to act as. Behind the `testing` feature; never
//! in product code.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    clippy::panic,
    clippy::print_stderr,
    reason = "test helpers fail the test by panicking and say what they did on stderr"
)]

use std::ffi::c_void;
use std::fmt::Write as _;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::process::Command;
use std::ptr;

use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, CreateRestrictedToken,
    CreateWellKnownSid, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
    GetSecurityDescriptorControl, ImpersonateLoggedOnUser, LOGON32_LOGON_NETWORK,
    LOGON32_PROVIDER_DEFAULT, LogonUserW, PSECURITY_DESCRIPTOR, RevertToSelf, SE_DACL_PROTECTED,
    SECURITY_MAX_SID_SIZE, SID_AND_ATTRIBUTES, TOKEN_DUPLICATE, TOKEN_QUERY, WELL_KNOWN_SID_TYPE,
    WinAuthenticatedUserSid, WinBuiltinUsersSid, WinInteractiveSid, WinWorldSid,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING};
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

use super::{LocalBox, TokenUserBuf, process_token, wide_nul};

/// Broad groups the user belongs to: a token restricted to these can do what "everyone logged
/// on" may do, but nothing granted to the user's own SID.
const BROAD_GROUPS: [WELL_KNOWN_SID_TYPE; 4] = [
    WinWorldSid,
    WinAuthenticatedUserSid,
    WinBuiltinUsersSid,
    WinInteractiveSid,
];

fn well_known_sid(kind: WELL_KNOWN_SID_TYPE) -> Vec<u64> {
    let mut buf = vec![0u64; (SECURITY_MAX_SID_SIZE as usize).div_ceil(8)];
    let mut len = SECURITY_MAX_SID_SIZE;
    // SAFETY: `buf` holds SECURITY_MAX_SID_SIZE bytes; no domain SID is needed for these.
    let ok =
        unsafe { CreateWellKnownSid(kind, ptr::null_mut(), buf.as_mut_ptr().cast(), &raw mut len) };
    assert_ne!(
        ok,
        0,
        "CreateWellKnownSid({kind}): {}",
        io::Error::last_os_error()
    );
    buf
}

/// Opens `path` with `access` from a thread impersonating a restricted copy of this process's
/// token whose restricting SIDs are the broad groups, plus the user's SID when `include_user`.
///
/// # Errors
///
/// The OS error of the open, if any.
pub fn open_restricted(path: &Path, access: u32, include_user: bool) -> io::Result<()> {
    let path = wide_nul(path.as_os_str());
    std::thread::spawn(move || {
        let token = process_token(TOKEN_DUPLICATE | TOKEN_QUERY)?;
        let user = TokenUserBuf::read(&token)?;
        let groups: Vec<Vec<u64>> = BROAD_GROUPS.iter().map(|k| well_known_sid(*k)).collect();
        let mut restrict: Vec<SID_AND_ATTRIBUTES> = groups
            .iter()
            .map(|g| SID_AND_ATTRIBUTES {
                Sid: g.as_ptr().cast_mut().cast(),
                Attributes: 0,
            })
            .collect();
        if include_user {
            restrict.push(SID_AND_ATTRIBUTES {
                Sid: user.sid(),
                Attributes: 0,
            });
        }
        let mut raw: HANDLE = ptr::null_mut();
        // SAFETY: `token` is valid with TOKEN_DUPLICATE; `restrict` points at SIDs that outlive
        // the call; `raw` is a valid out-pointer.
        let ok = unsafe {
            CreateRestrictedToken(
                token.as_raw_handle(),
                0,
                0,
                ptr::null(),
                0,
                ptr::null(),
                u32::try_from(restrict.len()).unwrap(),
                restrict.as_ptr(),
                &raw mut raw,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: a fresh token handle that nothing else owns.
        let restricted = unsafe { OwnedHandle::from_raw_handle(raw) };
        open_impersonating(&restricted, &path, access)
    })
    .join()
    .unwrap()
}

/// Opens the NUL-terminated `path` with `access` while this thread impersonates `token`.
///
/// # Errors
///
/// The OS error of the impersonation or the open.
pub fn open_impersonating(token: &OwnedHandle, path: &[u16], access: u32) -> io::Result<()> {
    // SAFETY: impersonation is undone below on this same thread, before it returns.
    if unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `path` is NUL-terminated; null security attributes and template are allowed.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            0,
            ptr::null(),
            OPEN_EXISTING,
            0,
            ptr::null_mut(),
        )
    };
    let result = if handle == INVALID_HANDLE_VALUE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: a fresh file handle that nothing else owns; dropping it closes it.
        drop(unsafe { OwnedHandle::from_raw_handle(handle) });
        Ok(())
    };
    // SAFETY: ends this thread's impersonation.
    let reverted = unsafe { RevertToSelf() };
    assert_ne!(reverted, 0, "RevertToSelf failed");
    result
}

/// Asserts `result` is an access-denied error.
pub fn assert_access_denied(result: io::Result<()>) {
    let err = result.expect_err("the open should have been refused");
    assert_eq!(
        err.raw_os_error(),
        Some(i32::try_from(ERROR_ACCESS_DENIED).unwrap()),
        "{err}"
    );
}

/// Asserts that the DACL of the open object `object` (a file, folder or pipe) is protected from
/// inheritance and holds exactly one ACE: an allow entry for the current user with `mask`.
pub fn assert_owner_only_dacl(object: &impl AsRawHandle, mask: u32) {
    let mut dacl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is open with READ_CONTROL; the out pointers are valid; `descriptor` is
    // freed by `_owned`.
    let rc = unsafe {
        GetSecurityInfo(
            object.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &raw mut dacl,
            ptr::null_mut(),
            &raw mut descriptor,
        )
    };
    assert_eq!(
        rc,
        0,
        "GetSecurityInfo: {}",
        io::Error::from_raw_os_error(rc.cast_signed())
    );
    // SAFETY: GetSecurityInfo allocated `descriptor` with LocalAlloc.
    let _owned = unsafe { LocalBox::from_raw(descriptor) };
    assert!(!dacl.is_null(), "a null DACL grants everyone everything");

    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `descriptor` is valid; both out-pointers are valid.
    let ok =
        unsafe { GetSecurityDescriptorControl(descriptor, &raw mut control, &raw mut revision) };
    assert_ne!(
        ok,
        0,
        "GetSecurityDescriptorControl: {}",
        io::Error::last_os_error()
    );
    assert_ne!(
        u32::from(control) & u32::from(SE_DACL_PROTECTED),
        0,
        "the DACL still inherits from the parent"
    );

    let mut info = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` points into `descriptor`; `info` is the right size for the class.
    let ok = unsafe {
        GetAclInformation(
            dacl,
            (&raw mut info).cast(),
            u32::try_from(size_of::<ACL_SIZE_INFORMATION>()).unwrap(),
            AclSizeInformation,
        )
    };
    assert_ne!(ok, 0, "GetAclInformation: {}", io::Error::last_os_error());
    assert_eq!(info.AceCount, 1, "the DACL must hold only the user's ACE");

    let mut ace: *mut c_void = ptr::null_mut();
    // SAFETY: index 0 exists (AceCount is 1); `ace` points into the DACL.
    let ok = unsafe { GetAce(dacl, 0, &raw mut ace) };
    assert_ne!(ok, 0, "GetAce: {}", io::Error::last_os_error());
    // SAFETY: the ACE type is checked before the ACE is read as ACCESS_ALLOWED_ACE.
    let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    assert_eq!(u32::from(ace.Header.AceType), ACCESS_ALLOWED_ACE_TYPE);
    assert_eq!(ace.Mask, mask);

    let token = process_token(TOKEN_QUERY).unwrap();
    let user = TokenUserBuf::read(&token).unwrap();
    // SAFETY: both are valid SIDs; the ACE's SID starts at SidStart.
    let same = unsafe { EqualSid((&raw const ace.SidStart).cast_mut().cast(), user.sid()) };
    assert_ne!(same, 0, "the ACE is not for the current user");
}

/// Logs the local account `name` on over the network (no "log on locally" right needed) and
/// returns its token.
///
/// # Errors
///
/// The OS error of the logon.
pub fn logon_local_user(name: &str, password: &str) -> io::Result<OwnedHandle> {
    let (name, password) = (wide_nul(name.as_ref()), wide_nul(password.as_ref()));
    let local = wide_nul(".".as_ref());
    let mut raw: HANDLE = ptr::null_mut();
    // SAFETY: the three strings are NUL-terminated and outlive the call; `raw` is a valid
    // out-pointer.
    let ok = unsafe {
        LogonUserW(
            name.as_ptr(),
            local.as_ptr(),
            password.as_ptr(),
            LOGON32_LOGON_NETWORK,
            LOGON32_PROVIDER_DEFAULT,
            &raw mut raw,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: on success `raw` is a fresh token handle that nothing else owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
}

/// Ends this thread's impersonation when dropped, also while a panic unwinds.
struct Impersonation;

impl Impersonation {
    fn start(token: &OwnedHandle) -> io::Result<Self> {
        // SAFETY: the token is a valid impersonation token; `Drop` undoes this on the same thread.
        if unsafe { ImpersonateLoggedOnUser(token.as_raw_handle()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self)
    }
}

impl Drop for Impersonation {
    fn drop(&mut self) {
        // SAFETY: ends this thread's impersonation; nothing else can fail here.
        unsafe { RevertToSelf() };
    }
}

/// Runs `f` on a new thread that impersonates `token`, so every file, pipe and registry call `f`
/// makes is checked against that user. Sockets are not: a TCP peer's identity is not visible to
/// the server.
///
/// # Errors
///
/// The OS error of the impersonation (`f` has not run then).
pub fn run_as<T: Send>(token: &OwnedHandle, f: impl FnOnce() -> T + Send) -> io::Result<T> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _impersonating = Impersonation::start(token)?;
                Ok::<T, io::Error>(f())
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}

/// The environment variable that turns a missing second user into a failure: set it to `1` where
/// the runner is known to let a test make local accounts (the Windows workflows set it).
pub const REQUIRE_SECOND_USER: &str = "PUDDLE_REQUIRE_SECOND_USER";

/// A throwaway local account, deleted on drop.
pub struct LocalUser {
    name: String,
    password: String,
}

impl LocalUser {
    /// Creates an account with a random name and password.
    ///
    /// # Errors
    ///
    /// Why the account could not be made (`net user` output): the process may not create accounts.
    pub fn create() -> Result<Self, String> {
        let mut random = [0u8; 6];
        getrandom::fill(&mut random).map_err(|e| format!("no randomness: {e}"))?;
        let tag = random.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        });
        let user = Self {
            name: format!("pdl{tag}"),
            password: format!("Pd!{tag}aZ9"),
        };
        let made = Command::new("net")
            .args(["user", &user.name, &user.password, "/add"])
            .output()
            .map_err(|e| format!("cannot run `net user`: {e}"))?;
        if made.status.success() {
            Ok(user)
        } else {
            // Never the password: `net` does not echo it, and the command line is not printed.
            Err(format!(
                "`net user <name> <password> /add` failed ({}): {} {}",
                made.status,
                String::from_utf8_lossy(&made.stdout).trim(),
                String::from_utf8_lossy(&made.stderr).trim()
            ))
        }
    }
}

impl Drop for LocalUser {
    fn drop(&mut self) {
        let _ = Command::new("net")
            .args(["user", &self.name, "/delete"])
            .output();
    }
}

/// A real second local user and a logged-on token for it: what another person's process on the
/// same machine can do. Every attack in the hostile-guest tier's second-user cases goes through
/// [`SecondUser::open`] or [`SecondUser::run`], so the OS (not a mock) decides.
pub struct SecondUser {
    // Drop order: the token closes before the account is deleted.
    token: OwnedHandle,
    user: LocalUser,
}

impl SecondUser {
    /// Makes the account and logs it on.
    ///
    /// # Errors
    ///
    /// Why this process can't: the account, or its logon, failed.
    pub fn acquire() -> Result<Self, String> {
        let user = LocalUser::create()?;
        let token = logon_local_user(&user.name, &user.password)
            .map_err(|e| format!("the new account `{}` cannot log on: {e}", user.name))?;
        Ok(Self { token, user })
    }

    /// [`SecondUser::acquire`] for a test: `None` (after printing why) when the runner can't, or
    /// a panic when [`REQUIRE_SECOND_USER`] is `1`, so a runner that stops allowing it turns the
    /// tests red instead of silently skipping them.
    #[must_use]
    pub fn for_test() -> Option<Self> {
        let outcome = Self::acquire();
        let required = std::env::var_os(REQUIRE_SECOND_USER).is_some_and(|v| v == "1");
        match outcome {
            Ok(user) => {
                eprintln!("second user `{}` made and logged on", user.name());
                Some(user)
            }
            Err(why) if required => panic!("{REQUIRE_SECOND_USER}=1 but {why}"),
            Err(why) => {
                eprintln!("SKIPPED: no second user ({why}); set {REQUIRE_SECOND_USER}=1 to fail");
                None
            }
        }
    }

    /// The account's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.user.name
    }

    /// Opens `path` (a file, folder or pipe) with `access` as this user, then closes it.
    ///
    /// # Errors
    ///
    /// The OS error of the open.
    pub fn open(&self, path: &Path, access: u32) -> io::Result<()> {
        open_impersonating(&self.token, &wide_nul(path.as_os_str()), access)
    }

    /// Runs `f` on a thread acting as this user (see [`run_as`]).
    ///
    /// # Panics
    ///
    /// When the impersonation fails.
    pub fn run<T: Send>(&self, f: impl FnOnce() -> T + Send) -> T {
        run_as(&self.token, f).expect("impersonating the second user")
    }
}

/// A folder every local user can walk into and read (under `%PUBLIC%`), removed on drop: a
/// second user needs it, because a folder in the test's own profile is closed to them and every
/// open would fail for that reason alone.
pub struct SharedDir(std::path::PathBuf);

impl SharedDir {
    /// A new, empty folder named after `prefix`.
    #[must_use]
    pub fn new(prefix: &str) -> Self {
        let public = std::env::var_os("PUBLIC").expect("%PUBLIC% is set on every Windows");
        let mut random = [0u8; 6];
        getrandom::fill(&mut random).unwrap();
        let tag = random.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        });
        let path = Path::new(&public).join(format!("{prefix}-{tag}"));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    /// The folder.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for SharedDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
