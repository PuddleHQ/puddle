// SPDX-License-Identifier: AGPL-3.0-or-later
//! The connection log the host gives its proxies: the store's, plus a standing problem for the
//! user while writing a record fails.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use puddle_types::{ConnectionEvent, ConnectionLog, Problem, Problems};

use crate::Store;

/// The key of the problem raised while connection records cannot be written.
pub const RECORD_PROBLEM: &str = "connection-records";

fn record_problem(reason: &str) -> Problem {
    Problem::new(
        RECORD_PROBLEM,
        "Puddle could not write to its activity log",
        format!(
            "{reason}. Connections still work, but the activity list is missing the ones made \
             since this started. A full disk is the usual cause: free some space."
        ),
    )
}

/// [`Store::record_connection`] as a [`ConnectionLog`]. A record that cannot be written is
/// dropped (the connection it describes has already been handled), logged, and raised once as the
/// problem [`RECORD_PROBLEM`], which ends with the next record that is written.
pub struct ConnectionRecorder {
    store: Arc<Store>,
    problems: Arc<Problems>,
    failing: AtomicBool,
}

impl ConnectionRecorder {
    /// Records into `store`, raising problems in `problems`.
    #[must_use]
    pub fn new(store: Arc<Store>, problems: Arc<Problems>) -> Self {
        Self {
            store,
            problems,
            failing: AtomicBool::new(false),
        }
    }

    /// The store it records into.
    #[must_use]
    pub fn store(&self) -> &Arc<Store> {
        &self.store
    }
}

impl ConnectionLog for ConnectionRecorder {
    fn record(&self, event: &ConnectionEvent) {
        match self.store.record_connection(event) {
            Ok(()) => {
                // A flag first, so the common case takes no lock.
                if self.failing.swap(false, Ordering::Relaxed) {
                    self.problems.clear(RECORD_PROBLEM);
                }
            }
            Err(err) => {
                tracing::warn!(origin = %event.origin, error = %err, "connection record not written");
                if !self.failing.swap(true, Ordering::Relaxed) {
                    self.problems.raise(record_problem(&err.to_string()));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
