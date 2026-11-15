// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_types::{
    CollectingSink, ConnectionDecision, ConnectionReason, EgressRequest, Event, Host, WorkspaceId,
};

use super::*;
use crate::{Limits, ManualClock};

fn event() -> ConnectionEvent {
    let request = EgressRequest::new(
        WorkspaceId::new("box").unwrap(),
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
