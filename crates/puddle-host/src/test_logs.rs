// SPDX-License-Identifier: AGPL-3.0-or-later
//! What code under test logs, for a unit test that must see the fields of a log line: a field's
//! expression runs only when a subscriber listens, so a test that installs none never runs it.

use std::io::Write;
use std::sync::{Arc, Mutex};

use tracing::Dispatch;

/// The text the subscriber of [`capture`] wrote.
#[derive(Clone, Default)]
pub(crate) struct Logs(Arc<Mutex<Vec<u8>>>);

impl Logs {
    pub(crate) fn text(&self) -> String {
        Sink(Arc::clone(&self.0)).flush().unwrap();
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

struct Sink(Arc<Mutex<Vec<u8>>>);

impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A subscriber for one test, at every level, and where its text goes. Run sync code under
/// `tracing::dispatcher::with_default(&dispatch, ..)` and a future of a current-thread runtime
/// under `tracing::instrument::WithSubscriber::with_subscriber`; neither touches other tests.
pub(crate) fn capture() -> (Dispatch, Logs) {
    let logs = Logs::default();
    let writer = Arc::clone(&logs.0);
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || Sink(Arc::clone(&writer)))
        .finish();
    (Dispatch::new(subscriber), logs)
}
