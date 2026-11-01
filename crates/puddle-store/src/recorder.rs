// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use puddle_types::{
        CollectingSink, ConnectionDecision, ConnectionReason, EgressRequest, Event, Host,
        WorkspaceName,
    };

    use super::*;
    use crate::{Limits, ManualClock};

    fn event() -> ConnectionEvent {
        let request = EgressRequest::new(
            WorkspaceName::new("box").unwrap(),
            Host::parse_normalised("api.example.com").unwrap(),
            443,
        );
        ConnectionEvent::new(&request, ConnectionDecision::Allow, ConnectionReason::Rule)
    }

    #[test]
    fn the_recorder_hands_out_the_store_it_records_into() {
        let store = Arc::new(
            Store::open_in_memory(Arc::new(ManualClock::new(1_000)), Limits::default()).unwrap(),
        );
        let problems = Arc::new(Problems::new(Arc::new(CollectingSink::default())));
        let log = ConnectionRecorder::new(store.clone(), problems);
        log.record(&event());
        assert!(Arc::ptr_eq(log.store(), &store));
        assert_eq!(store.audit_lines(0, 10).unwrap().len(), 1);
    }

    #[test]
    fn a_record_that_cannot_be_written_raises_one_problem_that_ends_with_the_next_write() {
        let store = Arc::new(
            Store::open_in_memory(Arc::new(ManualClock::new(1_000)), Limits::default()).unwrap(),
        );
        let sink = Arc::new(CollectingSink::default());
        let problems = Arc::new(Problems::new(sink.clone()));
        let log = ConnectionRecorder::new(store.clone(), problems.clone());

        log.record(&event());
        assert!(problems.list().is_empty(), "a good write raises nothing");

        store.fail_audit_writes("connection");
        log.record(&event());
        assert_eq!(
            problems.list().len(),
            1,
            "the first failure already raises it"
        );
        log.record(&event());
        let listed = problems.list();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].key, RECORD_PROBLEM);
        assert!(listed[0].detail.contains("disk full"), "{:?}", listed[0]);
        assert_eq!(
            sink.take(),
            [Event::ProblemsChanged {}],
            "two failures are announced once"
        );

        store.audit_writes_work_again();
        log.record(&event());
        assert!(
            problems.list().is_empty(),
            "the next write ends the problem"
        );
        assert_eq!(sink.take(), [Event::ProblemsChanged {}]);
    }
}
