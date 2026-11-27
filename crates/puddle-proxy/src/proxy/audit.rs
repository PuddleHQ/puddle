// SPDX-License-Identifier: AGPL-3.0-or-later
//! The audit record of one `CONNECT` connection, written even when the connection is cut.

use std::sync::{Arc, Mutex, PoisonError};

use puddle_types::ConnectionEvent;

use crate::counted::ByteCounts;

use super::Proxy;

/// Owns the audit record of one guest connection until it is written.
///
/// The connection's task can end without returning: the agent session ends or the route stops and
/// the task is aborted mid-splice, or the task panics. Dropping the guard still armed then hands
/// the latest snapshot to the connection log, with the bytes that had passed, so every connection
/// leaves one record (06 step 9). [`Self::finish`] writes the final event and disarms it.
pub(super) struct AuditGuard {
    proxy: Arc<Proxy>,
    counts: Arc<ByteCounts>,
    snapshot: Mutex<Option<ConnectionEvent>>,
}

impl AuditGuard {
    pub(super) fn new(proxy: &Arc<Proxy>, counts: &Arc<ByteCounts>) -> Self {
        Self {
            proxy: Arc::clone(proxy),
            counts: Arc::clone(counts),
            snapshot: Mutex::new(None),
        }
    }

    /// Replaces the snapshot a cut would write.
    pub(super) fn snapshot(&self, event: &ConnectionEvent) {
        *self.snapshot.lock().unwrap_or_else(PoisonError::into_inner) = Some(event.clone());
    }

    /// Writes `event` with the byte counts and disarms the guard.
    pub(super) async fn finish(&self, mut event: ConnectionEvent) {
        self.disarm();
        self.count_into(&mut event);
        self.proxy.record(event).await;
    }

    fn disarm(&self) -> Option<ConnectionEvent> {
        self.snapshot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn count_into(&self, event: &mut ConnectionEvent) {
        event.bytes_up = self.counts.read();
        event.bytes_down = self.counts.written();
    }
}

impl Drop for AuditGuard {
    fn drop(&mut self) {
        if let Some(mut event) = self.disarm() {
            self.count_into(&mut event);
            self.proxy.record_detached(event);
        }
    }
}

#[cfg(test)]
mod tests;
