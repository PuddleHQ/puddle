// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log lines the code under test wrote, for tests of a path whose only visible effect is one
//! line (a peer that dropped, a stream that ended with an error).

use std::sync::{Arc, Mutex, PoisonError};

use tracing::subscriber::DefaultGuard;

/// The text logged so far; clones share it.
#[derive(Clone, Default)]
pub struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// Everything logged since [`capture_logs`], one line per event.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(PoisonError::into_inner)).into_owned()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Records events at debug level and above on the calling thread until the guard is dropped. A
/// `#[tokio::test]` runs all its tasks on that thread; a multi-thread runtime would not be seen.
#[must_use]
pub fn capture_logs() -> (CapturedLogs, DefaultGuard) {
    let captured = CapturedLogs::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    (captured, tracing::subscriber::set_default(subscriber))
}

#[cfg(test)]
mod tests;
