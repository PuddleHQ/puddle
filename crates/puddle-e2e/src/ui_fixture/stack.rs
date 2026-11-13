// SPDX-License-Identifier: AGPL-3.0-or-later
//! The main-thread stack of the fixture backend.
//!
//! A stack overflow is an abort with no message (`thread 'main' has overflowed its stack`, exit
//! 0xC00000FD), so a program that lost its `/STACK` link argument (`build.rs`) would fail like a
//! crash. [`check_main_thread`] runs first in `main` and fails with the cause instead.

use std::io::Write;
use std::process::ExitCode;

/// The stack the program links on Windows (`build.rs`), in bytes.
#[must_use]
pub fn linked_stack_bytes() -> usize {
    env!("PUDDLE_MAIN_STACK_BYTES")
        .parse()
        .unwrap_or(usize::MAX)
}

/// Fails with a readable message when the calling thread has less stack than the program links.
/// Only Windows has a small default (1 MiB); elsewhere the main thread has 8 MiB and this passes.
///
/// # Errors
///
/// The size found and the size needed, and where the size is set.
pub fn check_main_thread() -> Result<(), String> {
    #[cfg(windows)]
    {
        let found = windows::current_thread_stack_bytes();
        let needed = linked_stack_bytes();
        if found < needed {
            return Err(format!(
                "the main thread has {found} bytes of stack but the start-up needs {needed}: the \
                 /STACK link argument of crates/puddle-e2e/build.rs did not reach this binary"
            ));
        }
    }
    Ok(())
}

/// Runs `serve` unless `check` fails; a failure is written to `stderr` and is exit code 2, so a
/// stack that is too small is named before the runtime starts instead of aborting inside it.
pub fn launch(
    check: impl FnOnce() -> Result<(), String>,
    serve: impl FnOnce() -> ExitCode,
    stderr: &mut impl Write,
) -> ExitCode {
    match check() {
        Ok(()) => serve(),
        Err(message) => finish(Err(message), stderr),
    }
}

/// The exit code of a finished run: success, or 2 with the message on `stderr`.
pub fn finish(result: Result<(), String>, stderr: &mut impl Write) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            // A closed stderr leaves nowhere to report to; the exit code still says it failed.
            let _ = writeln!(stderr, "puddle-ui-fixture: {message}");
            ExitCode::from(2)
        }
    }
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::System::Threading::GetCurrentThreadStackLimits;

    /// The reserved stack of the calling thread, in bytes.
    #[expect(
        unsafe_code,
        reason = "Win32 FFI: std has no call that reads the stack size of the running thread"
    )]
    pub(super) fn current_thread_stack_bytes() -> usize {
        let mut low = 0_usize;
        let mut high = 0_usize;
        // SAFETY: per Microsoft Learn (GetCurrentThreadStackLimits), two writable usize out
        // pointers.
        // https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getcurrentthreadstacklimits
        unsafe { GetCurrentThreadStackLimits(&raw mut low, &raw mut high) };
        high.saturating_sub(low)
    }
}

#[cfg(test)]
mod tests;
