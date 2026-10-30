// SPDX-License-Identifier: GPL-3.0-or-later
//! The forced race behind the rule against `try_recv()`: a host forwards an event from its own task, so the
//! event can land after the replies a test awaited (the sign-in failure that flaked on a loaded runner).
//! The emit is held back by 300 ms of virtual time, so the race is certain, not a matter of load: the old
//! one-shot read is red against it, the kit's read is green against the same emit.

use std::time::Duration;

use puddle_test_kit::{next_event_matching, no_event_matching};
use tokio::sync::broadcast;

const SIGN_IN_FAILED: &str = "sign-in failed";

/// A host whose forwarder task emits `event` 300 ms after the test's replies came back.
fn host_emitting_late(event: &'static str) -> broadcast::Receiver<&'static str> {
    let (tx, rx) = broadcast::channel(8);
    tokio::spawn(async move {
        // Simulated latency: the host's forwarder is slow, on virtual time.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = tx.send(event);
    });
    rx
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "left: []")]
async fn the_old_one_shot_read_is_red_against_a_delayed_emit() {
    let mut events = host_emitting_late(SIGN_IN_FAILED);
    // The replies come back at once; the emit is still 300 ms away.
    tokio::task::yield_now().await;
    let mut failures = Vec::new();
    // flake-ok: this is the shape the rule forbids, kept here to show that it fails against the delayed emit.
    while let Ok(event) = events.try_recv() {
        failures.push(event);
    }
    assert_eq!(failures, [SIGN_IN_FAILED]);
}

#[tokio::test(start_paused = true)]
async fn the_kit_read_is_green_against_the_same_delayed_emit() {
    let mut events = host_emitting_late(SIGN_IN_FAILED);
    tokio::task::yield_now().await;
    let failure = next_event_matching(
        "the sign-in failure",
        Duration::from_secs(10),
        &mut events,
        Some,
    )
    .await;
    assert_eq!(failure, SIGN_IN_FAILED);
    no_event_matching(
        "a second failure",
        Duration::from_millis(500),
        &mut events,
        Some,
    )
    .await;
}
