// SPDX-License-Identifier: AGPL-3.0-or-later
//! Windows test helpers: the pseudoconsole child, console control events and process watching.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
};
use windows_sys::Win32::System::Console::{
    AttachConsole, COORD, CTRL_C_EVENT, ClosePseudoConsole, CreatePseudoConsole, FreeConsole,
    GenerateConsoleCtrlEvent, HPCON, SetConsoleCtrlHandler,
};
use windows_sys::Win32::System::Pipes::CreatePipe;
use windows_sys::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, DeleteProcThreadAttributeList,
    EXTENDED_STARTUPINFO_PRESENT, GetExitCodeProcess, INFINITE, InitializeProcThreadAttributeList,
    LPPROC_THREAD_ATTRIBUTE_LIST, OpenProcess, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
    PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
    STARTF_USESTDHANDLES, STARTUPINFOEXW, TerminateProcess, UpdateProcThreadAttribute,
    WaitForSingleObject,
};
use windows_sys::core::BOOL;

/// Makes Ctrl-C reach this process's handlers even if its parent disabled it (a process
/// started in a new process group ignores Ctrl-C, and children inherit that).
pub(crate) fn process_ctrl_c() {
    // SAFETY: a null handler with FALSE only clears the ignore flag.
    unsafe { SetConsoleCtrlHandler(None, 0) };
}

/// Attaches to `target`'s console and sends Ctrl-C to it. Returns the helper's exit code:
/// 0 sent, 4 attach failed, 5 send failed.
pub(crate) fn send_ctrl_c(target: u32) -> i32 {
    // SAFETY: plain console calls without pointers; this helper process does nothing else.
    unsafe {
        FreeConsole();
        if AttachConsole(target) == 0 {
            return 4;
        }
        // Ignore it here; every other process on the console gets it.
        SetConsoleCtrlHandler(None, 1);
        let sent = GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0);
        FreeConsole();
        if sent == 0 { 5 } else { 0 }
    }
}

fn check(ok: BOOL) -> std::io::Result<()> {
    if ok == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// An anonymous pipe: (read end, write end), neither inheritable.
fn pipe() -> std::io::Result<(OwnedHandle, OwnedHandle)> {
    let (mut read, mut write) = (std::ptr::null_mut(), std::ptr::null_mut());
    // SAFETY: two out pointers to locals; no security attributes.
    check(unsafe { CreatePipe(&raw mut read, &raw mut write, std::ptr::null(), 0) })?;
    // SAFETY: both handles were just created and are owned from here on.
    Ok(unsafe {
        (
            OwnedHandle::from_raw_handle(read),
            OwnedHandle::from_raw_handle(write),
        )
    })
}

/// Opens a pseudoconsole and drains its screen output on a thread (a console whose output
/// isn't read blocks); nothing in that output matters here.
fn open_console() -> std::io::Result<HPCON> {
    let (console_in, input) = pipe()?;
    let (screen, console_out) = pipe()?;
    let mut console: HPCON = 0;
    // SAFETY: valid pipe handles and an out pointer to a local.
    let hr = unsafe {
        CreatePseudoConsole(
            COORD { X: 120, Y: 30 },
            console_in.as_raw_handle(),
            console_out.as_raw_handle(),
            0,
            &raw mut console,
        )
    };
    if hr < 0 {
        return Err(std::io::Error::from_raw_os_error(hr));
    }
    // The pseudoconsole holds its own duplicates of its ends.
    drop((console_in, console_out));
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut std::fs::File::from(screen), &mut std::io::sink());
        drop(input);
    });
    Ok(console)
}

/// `"exe" args...`, NUL-terminated UTF-16 (the arguments need no quoting).
fn command_line(exe: &std::path::Path, args: &[&str]) -> Vec<u16> {
    std::iter::once(format!("\"{}\"", exe.display()))
        .chain(args.iter().map(|a| (*a).to_owned()))
        .collect::<Vec<_>>()
        .join(" ")
        .encode_utf16()
        .chain([0])
        .collect()
}

/// This process's environment with `vars` set, as a sorted UTF-16 environment block.
fn env_block(vars: &[(&str, &str)]) -> Vec<u16> {
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = std::env::vars_os()
        .filter(|(k, _)| !vars.iter().any(|(v, _)| k.eq_ignore_ascii_case(v)))
        .collect();
    env.extend(vars.iter().map(|(k, v)| ((*k).into(), (*v).into())));
    env.sort_by_key(|(k, _)| k.to_ascii_uppercase());
    let mut block: Vec<u16> = Vec::new();
    for (k, v) in &env {
        block.extend(k.encode_wide());
        block.push(u16::from(b'='));
        block.extend(v.encode_wide());
        block.push(0);
    }
    block.push(0);
    block
}

/// A process started inside a pseudoconsole the test owns (what Windows Terminal does for
/// a tab). Its stdout and stderr go to a plain pipe, not through the pseudoconsole.
pub(crate) struct PtyChild {
    pid: u32,
    process: OwnedHandle,
    console: Option<HPCON>,
}

impl PtyChild {
    /// Starts `exe args` with `vars` added to this process's environment. Returns the
    /// child and the read end of its stdout/stderr.
    pub(crate) fn spawn(
        exe: &std::path::Path,
        args: &[&str],
        vars: &[(&str, &str)],
    ) -> std::io::Result<(Self, std::fs::File)> {
        let console = open_console()?;

        // The child's stdout and stderr: the write end, inherited (and only it).
        let (out_read, out_write) = pipe()?;
        // SAFETY: a handle owned above.
        check(unsafe {
            SetHandleInformation(
                out_write.as_raw_handle(),
                HANDLE_FLAG_INHERIT,
                HANDLE_FLAG_INHERIT,
            )
        })?;

        let mut size = 0usize;
        // SAFETY: the documented size query (fails with ERROR_INSUFFICIENT_BUFFER).
        unsafe { InitializeProcThreadAttributeList(std::ptr::null_mut(), 2, 0, &raw mut size) };
        let mut list = vec![0u8; size];
        let attrs: LPPROC_THREAD_ATTRIBUTE_LIST = list.as_mut_ptr().cast();
        // SAFETY: `list` is `size` bytes and outlives every use of `attrs`.
        check(unsafe { InitializeProcThreadAttributeList(attrs, 2, 0, &raw mut size) })?;
        let inherit = [out_write.as_raw_handle()];
        // SAFETY: the values live until CreateProcessW below returns.
        let updated = unsafe {
            check(UpdateProcThreadAttribute(
                attrs,
                0,
                PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE as usize,
                console as *const c_void,
                size_of::<HPCON>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ))
            .and_then(|()| {
                check(UpdateProcThreadAttribute(
                    attrs,
                    0,
                    PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                    inherit.as_ptr().cast(),
                    size_of_val(&inherit),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                ))
            })
        };

        // SAFETY: a plain C struct for which all zeroes is valid.
        let mut startup: STARTUPINFOEXW = unsafe { std::mem::zeroed() };
        startup.StartupInfo.cb = u32::try_from(size_of::<STARTUPINFOEXW>()).unwrap();
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdOutput = out_write.as_raw_handle();
        startup.StartupInfo.hStdError = out_write.as_raw_handle();
        startup.lpAttributeList = attrs;

        let mut line = command_line(exe, args);
        let block = env_block(vars);

        // SAFETY: a plain C struct for which all zeroes is valid.
        let mut info: PROCESS_INFORMATION = unsafe { std::mem::zeroed() };
        let created = updated.and_then(|()| {
            // SAFETY: every pointer refers to a local that outlives the call.
            check(unsafe {
                CreateProcessW(
                    std::ptr::null(),
                    line.as_mut_ptr(),
                    std::ptr::null(),
                    std::ptr::null(),
                    1,
                    EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT,
                    block.as_ptr().cast(),
                    std::ptr::null(),
                    &raw const startup.StartupInfo,
                    &raw mut info,
                )
            })
        });
        // SAFETY: initialised above; nothing uses it after this.
        unsafe { DeleteProcThreadAttributeList(attrs) };
        drop(list);
        drop(out_write);
        if let Err(e) = created {
            // SAFETY: created above, closed once.
            unsafe { ClosePseudoConsole(console) };
            return Err(e);
        }
        // SAFETY: CreateProcessW returned both handles; the thread one isn't needed.
        let process = unsafe {
            CloseHandle(info.hThread);
            OwnedHandle::from_raw_handle(info.hProcess)
        };
        Ok((
            Self {
                pid: info.dwProcessId,
                process,
                console: Some(console),
            },
            std::fs::File::from(out_read),
        ))
    }

    pub(crate) fn id(&self) -> u32 {
        self.pid
    }

    /// Closes the pseudoconsole: every process on it gets `CTRL_CLOSE_EVENT`. Doesn't wait
    /// (the close itself may wait for the clients on newer Windows builds).
    pub(crate) fn close_console(&mut self) {
        if let Some(console) = self.console.take() {
            // SAFETY: created in `spawn`, closed once.
            std::thread::spawn(move || unsafe { ClosePseudoConsole(console) });
        }
    }

    pub(crate) fn kill(&mut self) -> std::io::Result<()> {
        // SAFETY: the process handle owned by `self`.
        check(unsafe { TerminateProcess(self.process.as_raw_handle(), 1) })
    }

    pub(crate) fn wait(&mut self) -> Option<i32> {
        let mut code = 0u32;
        // SAFETY: the process handle owned by `self`; an out pointer to a local.
        unsafe {
            WaitForSingleObject(self.process.as_raw_handle(), INFINITE);
            if GetExitCodeProcess(self.process.as_raw_handle(), &raw mut code) == 0 {
                return None;
            }
        }
        Some(code.cast_signed())
    }
}

impl Drop for PtyChild {
    fn drop(&mut self) {
        self.close_console();
    }
}

/// Process handles opened now, waited on later (so an exit in between isn't missed).
pub(crate) struct Watch(Vec<(u32, HANDLE)>);

impl Watch {
    pub(crate) fn new(pids: &[u32]) -> Self {
        Self(
            pids.iter()
                .map(|&pid| {
                    // SAFETY: OpenProcess has no pointer arguments; a null result (already
                    // gone) is handled by the waiter.
                    (pid, unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) })
                })
                .collect(),
        )
    }

    pub(crate) fn all_gone_within(&self, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        self.0.iter().all(|&(_, handle)| {
            if handle.is_null() {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let ms = u32::try_from(left.as_millis()).unwrap_or(u32::MAX);
            // SAFETY: a live process handle opened in `new`.
            unsafe { WaitForSingleObject(handle, ms) == WAIT_OBJECT_0 }
        })
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        for &(_, handle) in &self.0 {
            if !handle.is_null() {
                // SAFETY: opened in `new`, closed once.
                unsafe { CloseHandle(handle) };
            }
        }
    }
}
