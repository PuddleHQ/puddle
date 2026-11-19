// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starts a child process the one way Puddle's host does: on Windows, without a console window.
//!
//! The desktop app is a Windows GUI program, so it has no console. When such a process starts a
//! console program (`git`, `msb`, PowerShell, `az`), Windows gives the child a console of its own,
//! and that console's window shows on the user's desktop for as long as the child runs: a version
//! check flashes a terminal window. [`command`] sets `CREATE_NO_WINDOW`, which gives the child a
//! console that has no window. A child of a process that has a console (`puddle doctor` in a
//! terminal) is started the same way; it then writes through its pipes, never to the terminal.
//!
//! Every host-side `Command::new` goes through here; `scripts/check-spawn.py` (the `spawn` gate)
//! fails one that does not. On other systems [`command`] is `Command::new`.
#![forbid(unsafe_code)]

use std::ffi::OsStr;
use std::process::Command;

/// The process creation flags [`command`] sets on Windows. A caller that sets its own flags with
/// `creation_flags` replaces these, so it must include them.
#[cfg(windows)]
pub const CREATION_FLAGS: u32 = windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

/// A [`Command`] for `program` that opens no console window on Windows.
///
/// For tokio, wrap it: `tokio::process::Command::from(puddle_process::command(program))`.
#[must_use]
pub fn command(program: impl AsRef<OsStr>) -> Command {
    #[cfg_attr(not(windows), expect(unused_mut, reason = "only Windows sets flags"))]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATION_FLAGS);
    }
    command
}

#[cfg(test)]
mod tests;
