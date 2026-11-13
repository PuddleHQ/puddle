// SPDX-License-Identifier: AGPL-3.0-or-later
//! Owner-only files and folders: the one place that knows how each OS says "only this user".
//!
//! | OS | Folder | File | [`check`] |
//! |---|---|---|---|
//! | Unix | new folders `0700` | `0600`, created exclusively | refuses a file with any group or other bit |
//! | Windows | new folders get a protected ACL with one entry, full control for the current user, which their children inherit | the same ACL, set when the file is created | refuses any ACL that is missing or has an entry for another account or a non-allow entry |
//!
//! Every caller (the API's connection file, the host's settings and registry files, the database,
//! the per-run endpoint folders) gets the same behaviour through this one module.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

#[cfg(not(any(unix, windows)))]
compile_error!("puddle-fs supports unix and windows hosts");

/// Creates `dir` and its missing parents, owner-only. An existing folder is left as it is.
///
/// # Errors
///
/// Any file-system error.
pub fn create_dir(dir: &Path) -> io::Result<()> {
    platform::create_dir(dir)
}

/// Creates the one folder `dir` owner-only, failing if it exists (a pre-created folder, or a
/// symlink in the last component, is never reused). The parent must exist. Unlike [`create_dir`]
/// this is for a folder whose whole point is that nobody else could have touched it first, such
/// as a per-run folder under a random name.
///
/// # Errors
///
/// Any file-system error, including `AlreadyExists`, or on Unix a mode that is not exactly `0700`
/// after creation.
pub fn create_new_dir(dir: &Path) -> io::Result<()> {
    platform::create_new_dir(dir)
}

/// Creates `path` owner-only, failing if it exists (so a file someone else pre-created, or a
/// symlink, is never written through).
///
/// # Errors
///
/// Any file-system error, including `AlreadyExists`.
pub fn create_file(path: &Path) -> io::Result<File> {
    platform::create_file(path)
}

/// How a file is open to others, as [`check`] reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exposed {
    /// Unix permission bits with a group or other bit set.
    Mode(u32),
    /// A Windows access list that isn't the current user's alone, and why.
    Acl(String),
}

impl fmt::Display for Exposed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mode(mode) => write!(f, "mode {mode:o}; it must be 0600"),
            Self::Acl(why) => write!(f, "{why}; it must be accessible to its owner only"),
        }
    }
}

/// Why [`check`] failed.
#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    /// The file's permissions couldn't be read.
    #[error("can't read the file's permissions: {0}")]
    Io(#[from] io::Error),
    /// Others may read it.
    #[error("readable by other users ({0})")]
    Exposed(Exposed),
}

/// Whether the open file `file` is owner-only.
///
/// # Errors
///
/// [`CheckError::Exposed`] if others may read it, [`CheckError::Io`] if its permissions can't be
/// read.
pub fn check(file: &File) -> Result<(), CheckError> {
    platform::check(file)
}

/// Makes the existing folder `dir` owner-only, creating it (owner-only) if it is missing.
///
/// Returns what was wrong before the fix, or `None` if the folder was already owner-only or has
/// just been created. On Unix it clears the group and other bits; on Windows it replaces the
/// folder's access list with the one-entry protected list, which the folder's existing children
/// inherit. It checks the result and fails if the folder is still exposed, so a caller can
/// refuse to run rather than run with wider permissions.
///
/// # Errors
///
/// Any file-system error (including not owning the folder), or `InvalidInput` if `dir` is not a
/// folder.
pub fn tighten_dir(dir: &Path) -> io::Result<Option<Exposed>> {
    if !dir.exists() {
        create_dir(dir)?;
        return Ok(None);
    }
    platform::tighten_dir(dir)
}

/// Makes the existing file `path` owner-only, like [`tighten_dir`]. A missing file is not an
/// error and returns `None`, and neither is one that disappears while it is being looked at (a
/// database's `-wal` and `-shm` files go when its last connection closes); a file that isn't a
/// regular file (a symlink, a folder) is refused.
///
/// Windows reports a file whose deletion is under way (it is deleted, but a handle to it is
/// still open) as "access denied" instead of "not found". A denial is therefore retried for up
/// to two seconds: the file is gone by then (`None`), or the denial is real and is returned.
///
/// # Errors
///
/// Any file-system error, or `InvalidInput` if `path` is not a regular file.
pub fn tighten_file(path: &Path) -> io::Result<Option<Exposed>> {
    tighten_file_within(DELETE_GRACE, || platform::tighten_file(path))
}

/// How long a denied file gets to finish being deleted before the denial counts.
const DELETE_GRACE: Duration = Duration::from_secs(2);
const DELETE_POLL: Duration = Duration::from_millis(10);

/// Runs `attempt` (one tightening of one file) until it is not denied or `grace` has passed.
/// A file that is gone, now or when retried, is nothing to tighten.
fn tighten_file_within(
    grace: Duration,
    mut attempt: impl FnMut() -> io::Result<Option<Exposed>>,
) -> io::Result<Option<Exposed>> {
    let started = Instant::now();
    loop {
        match attempt() {
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) if err.kind() == io::ErrorKind::PermissionDenied => {
                if started.elapsed() >= grace {
                    return Err(err);
                }
                thread::sleep(DELETE_POLL);
            }
            other => return other,
        }
    }
}

/// Writes `body` to `path` owner-only and atomically: a temporary owner-only file in the same
/// folder, synced, then renamed over `path`, so a reader never sees half of it. Creates the
/// folder if needed. The temporary file is removed on failure.
///
/// # Errors
///
/// Any file-system error, or the OS random source failing (it names the temporary file).
pub fn write_atomic(path: &Path, body: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    create_dir(dir)?;
    let mut suffix = [0u8; 8];
    getrandom::fill(&mut suffix).map_err(|err| io::Error::other(err.to_string()))?;
    let file_name = path
        .file_name()
        .map_or_else(|| "private".into(), |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{file_name}.{}.tmp", hex(&suffix)));
    let result = (|| {
        let mut file = create_file(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        // Best effort: the temporary file may hold a secret, so don't leave it behind.
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Puts the finished file `from` in place of the existing file `to` in one step. On Unix this is
/// a rename. On Windows it is `ReplaceFileW`, so `to` keeps the access list it had (a rename
/// would give it the access list of `from`, which was made owner-only or inherited).
///
/// # Errors
///
/// Any file-system error; `from` and `to` are left as they were.
pub fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    platform::replace_file(from, to)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

#[cfg(test)]
mod tests;
