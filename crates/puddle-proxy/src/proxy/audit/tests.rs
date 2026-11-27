// SPDX-License-Identifier: AGPL-3.0-or-later
//! The guard writes one record per connection, however the connection ends.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use puddle_netpolicy::normalise_host;
use puddle_types::{ConnectionDecision, ConnectionReason, EgressRequest, NullSink, WorkspaceId};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;
use crate::counted::Counted;
use crate::testing::{CollectingConnectionLog, StaticPolicy};

fn proxy() -> (Arc<Proxy>, Arc<CollectingConnectionLog>) {
    let log = Arc::new(CollectingConnectionLog::new());
    let proxy = Proxy::new(Arc::new(StaticPolicy::new()), Arc::new(NullSink))
        .with_connection_log(log.clone());
    (Arc::new(proxy), log)
}

fn event(decision: ConnectionDecision) -> ConnectionEvent {
    let request = EgressRequest::new(
        WorkspaceId::new("box").unwrap(),
        normalise_host("example.test").unwrap().into_host(),
        443,
    );
    ConnectionEvent::new(&request, decision, ConnectionReason::Rule)
}

/// Counts of a stream that read 5 bytes and wrote 3: distinct, so a swapped pair shows.
async fn counts() -> Arc<ByteCounts> {
    let (near, mut far) = tokio::io::duplex(64);
    let (mut counted, counts) = Counted::new(near);
    far.write_all(b"hello").await.unwrap();
    let mut buf = [0u8; 5];
    counted.read_exact(&mut buf).await.unwrap();
    counted.write_all(b"abc").await.unwrap();
    counts
}

async fn recorded(log: &CollectingConnectionLog, count: usize) -> Vec<ConnectionEvent> {
    // Past the last record a second would be a duplicate; waiting for `count + 1` shows none.
    log.wait_for(count + 1, Duration::from_millis(300)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn an_armed_guard_dropped_in_a_runtime_records_its_snapshot_with_the_bytes() {
    let (proxy, log) = proxy();
    let guard = AuditGuard::new(&proxy, &counts().await);
    guard.snapshot(&event(ConnectionDecision::Allow));
    drop(guard);
    let events = recorded(&log, 1).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].decision, ConnectionDecision::Allow);
    assert_eq!((events[0].bytes_up, events[0].bytes_down), (5, 3));
}

#[test]
fn an_armed_guard_dropped_outside_a_runtime_records_on_the_spot() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (proxy, log) = proxy();
    let counts = runtime.block_on(counts());
    let guard = AuditGuard::new(&proxy, &counts);
    guard.snapshot(&event(ConnectionDecision::Blocked));
    drop(guard);
    let events = log.events();
    assert_eq!(events.len(), 1, "written before drop returns");
    assert_eq!(events[0].decision, ConnectionDecision::Blocked);
    assert_eq!((events[0].bytes_up, events[0].bytes_down), (5, 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panic_with_the_guard_armed_still_leaves_the_record() {
    let (proxy, log) = proxy();
    let counts = counts().await;
    let unwound = catch_unwind(AssertUnwindSafe(|| {
        let guard = AuditGuard::new(&proxy, &counts);
        guard.snapshot(&event(ConnectionDecision::Allow));
        panic!("the connection task panics");
    }));
    assert!(unwound.is_err());
    let events = recorded(&log, 1).await;
    assert_eq!(events.len(), 1);
    assert_eq!((events[0].bytes_up, events[0].bytes_down), (5, 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_snapshot_is_the_one_a_cut_records() {
    let (proxy, log) = proxy();
    let guard = AuditGuard::new(&proxy, &counts().await);
    guard.snapshot(&event(ConnectionDecision::Blocked));
    guard.snapshot(&event(ConnectionDecision::Allow));
    drop(guard);
    let events = recorded(&log, 1).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].decision, ConnectionDecision::Allow);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guard_that_never_saw_a_connection_records_nothing() {
    let (proxy, log) = proxy();
    drop(AuditGuard::new(&proxy, &counts().await));
    assert_eq!(recorded(&log, 0).await.len(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn finish_records_the_final_event_once_with_the_bytes_and_disarms_the_drop() {
    let (proxy, log) = proxy();
    let guard = AuditGuard::new(&proxy, &counts().await);
    guard.snapshot(&event(ConnectionDecision::Blocked));
    guard.finish(event(ConnectionDecision::Allow)).await;
    assert_eq!(log.events().len(), 1, "written before finish returns");
    drop(guard);
    let events = recorded(&log, 1).await;
    assert_eq!(events.len(), 1, "the drop adds no second record");
    assert_eq!(events[0].decision, ConnectionDecision::Allow);
    assert_eq!((events[0].bytes_up, events[0].bytes_down), (5, 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn finish_without_a_snapshot_still_records() {
    let (proxy, log) = proxy();
    let guard = AuditGuard::new(&proxy, &counts().await);
    guard.finish(event(ConnectionDecision::Allow)).await;
    assert_eq!(log.events().len(), 1);
}
