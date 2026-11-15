// SPDX-License-Identifier: AGPL-3.0-or-later
//! Freezing a VM's process from a test, and the arithmetic of a heartbeat that ran through it.
//!
//! A frozen VMM gets no CPU, so the guest inside it stands still, the way it does while a host
//! sleeps. Only the VM process is frozen: the test, the host and the machine keep running, which
//! is why this is safe on any machine, unlike a real suspend. On Unix the freeze is `SIGSTOP`; on
//! Windows it is `NtSuspendProcess`, called through PowerShell because the workspace forbids
//! `unsafe` and a test is not worth an FFI crate.

use std::process::Command;

use crate::HarnessError;

/// What to do to the process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Freeze,
    Thaw,
}

/// Calls ntdll's suspend or resume on a process id. `Process.Handle` opens the process with full
/// access, which a process of the same user allows; a non-zero NTSTATUS fails the script.
fn windows_script(action: Action, pid: u32) -> String {
    let call = match action {
        Action::Freeze => "NtSuspendProcess",
        Action::Thaw => "NtResumeProcess",
    };
    format!(
        "$sig = '[DllImport(\"ntdll.dll\")] public static extern int NtSuspendProcess(IntPtr h); \
         [DllImport(\"ntdll.dll\")] public static extern int NtResumeProcess(IntPtr h);'; \
         Add-Type -MemberDefinition $sig -Name Nt -Namespace Puddle; \
         $p = [System.Diagnostics.Process]::GetProcessById({pid}); \
         $r = [Puddle.Nt]::{call}($p.Handle); \
         if ($r -ne 0) {{ Write-Error \"{call} returned $r\"; exit 1 }}"
    )
}

/// The program and arguments that do `action` to `pid` on the given OS family.
fn command_line(windows: bool, action: Action, pid: u32) -> (&'static str, Vec<String>) {
    if windows {
        (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                windows_script(action, pid),
            ],
        )
    } else {
        let signal = match action {
            Action::Freeze => "-STOP",
            Action::Thaw => "-CONT",
        };
        ("kill", vec![signal.into(), pid.to_string()])
    }
}

fn run(action: Action, pid: u32) -> Result<(), HarnessError> {
    let (program, args) = command_line(cfg!(windows), action, pid);
    let out = Command::new(program)
        .args(&args)
        .output()
        .map_err(|e| HarnessError::io("run", program, e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(HarnessError::Freeze {
            pid,
            detail: format!(
                "{program} {}: {}{}",
                out.status,
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            ),
        })
    }
}

/// A process that is frozen until [`FrozenProcess::thaw`] or the drop, so a test that panics
/// during the freeze does not leave a VM stopped behind.
#[derive(Debug)]
pub struct FrozenProcess {
    pid: u32,
    frozen: bool,
}

impl FrozenProcess {
    /// Freezes `pid`.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Freeze`] when the OS refuses (no such process, not allowed).
    pub fn freeze(pid: u32) -> Result<Self, HarnessError> {
        run(Action::Freeze, pid)?;
        Ok(Self { pid, frozen: true })
    }

    /// Lets the process run again.
    ///
    /// # Errors
    ///
    /// [`HarnessError::Freeze`] when the OS refuses; the process then stays marked frozen, so the
    /// drop tries once more.
    pub fn thaw(&mut self) -> Result<(), HarnessError> {
        run(Action::Thaw, self.pid)?;
        self.frozen = false;
        Ok(())
    }
}

impl Drop for FrozenProcess {
    fn drop(&mut self) {
        if self.frozen {
            // Best effort: a panic is already unwinding, and a frozen VM is cleaned up by the
            // harness's own kill.
            let _ = run(Action::Thaw, self.pid);
        }
    }
}

/// The ticks of a one-second heartbeat that ran through a pause: how many gaps longer than
/// `GAP_OVER` it holds and the longest one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gaps {
    /// Gaps between two ticks of more than `GAP_OVER` seconds.
    pub count: usize,
    /// The longest gap between two ticks, in seconds (0 for fewer than two ticks).
    pub longest: i64,
}

/// A heartbeat tick later than this after the previous one is a gap, not jitter.
const GAP_OVER: i64 = 2;

impl Gaps {
    /// The gaps in `ticks`, epoch seconds in the order they were written.
    #[must_use]
    pub fn of(ticks: &[i64]) -> Self {
        let mut count = 0;
        let mut longest = 0;
        for pair in ticks.windows(2) {
            let &[earlier, later] = pair else { continue };
            let gap = later.saturating_sub(earlier);
            if gap > GAP_OVER {
                count += 1;
            }
            longest = longest.max(gap);
        }
        Self { count, longest }
    }

    /// True for exactly one gap, as long as the pause within `GAP_SLACK_BEFORE` and
    /// `GAP_SLACK_AFTER` seconds: the bar of a real sleep check.
    #[must_use]
    pub fn match_pause(self, paused: i64) -> bool {
        self.count == 1
            && self.longest >= paused - GAP_SLACK_BEFORE
            && self.longest <= paused + GAP_SLACK_AFTER
    }
}

/// How much shorter than the pause the one gap may be (the guest's last tick comes before the
/// freeze and its first after the thaw).
const GAP_SLACK_BEFORE: i64 = 30;
/// How much longer than the pause the one gap may be (the guest needs time to wake).
const GAP_SLACK_AFTER: i64 = 120;

/// Parses a heartbeat file: one epoch second per line; anything else (a half-written last line)
/// is skipped.
#[must_use]
pub fn parse_ticks(text: &str) -> Vec<i64> {
    text.lines()
        .filter_map(|line| line.trim().parse().ok())
        .collect()
}

#[cfg(test)]
mod tests;
