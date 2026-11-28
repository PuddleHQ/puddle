// SPDX-License-Identifier: AGPL-3.0-or-later
//! A child process a test starts and must not leave behind.

use std::path::PathBuf;
use std::process::{Child, ExitStatus};
use std::time::{Duration, Instant};

/// How long a polite stop gets before the kill.
const STOP_WAIT: Duration = Duration::from_secs(10);

/// Owns a child process and ends it when dropped, also when the test panicked first: it asks for a
/// stop (the stop file, when one was given), waits at most [`STOP_WAIT`], then kills and reaps the
/// child. A child that already exited is only reaped.
#[derive(Debug)]
pub struct ProcessGuard {
    child: Child,
    stop_file: Option<PathBuf>,
    wait: Duration,
}

impl ProcessGuard {
    /// Guards `child`; `stop_file`, when given, is written on drop to ask the program to stop.
    #[must_use]
    pub fn new(child: Child, stop_file: Option<PathBuf>) -> Self {
        Self {
            child,
            stop_file,
            wait: STOP_WAIT,
        }
    }

    /// Replaces the bound on the polite stop; a test of the guard itself keeps it short.
    #[must_use]
    pub fn with_stop_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }

    /// The child, for a test that reads its pid or waits for its exit itself.
    pub fn child(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Waits for the child to exit on its own, for a test that checks its exit status; the guard
    /// still kills it on drop when this is not reached.
    ///
    /// # Errors
    /// The error of the operating system's wait.
    pub fn wait(&mut self) -> std::io::Result<ExitStatus> {
        self.child.wait()
    }
}

impl Drop for ProcessGuard {
    fn drop(&mut self) {
        if let Some(stop) = &self.stop_file {
            std::fs::write(stop, "").ok();
        }
        let deadline = Instant::now() + self.wait;
        while matches!(self.child.try_wait(), Ok(None)) && Instant::now() < deadline {
            // A bounded poll on a child process: std has no wait with a timeout.
            std::thread::sleep(Duration::from_millis(20));
        }
        if matches!(self.child.try_wait(), Ok(None)) {
            self.child.kill().ok();
            self.child.wait().ok();
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
