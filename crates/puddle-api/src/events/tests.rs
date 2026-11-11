// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;

use puddle_types::WorkspaceStatus;

use super::*;

fn name(s: &str) -> WorkspaceId {
    WorkspaceId::new(s).unwrap()
}

fn oom(s: &str) -> Event {
    Event::oom_kill(name(s), 1, "x")
}

#[test]
fn filter_keeps_its_workspace_and_global_events() {
    let a = name("a");
    assert!(wanted(&oom("a"), Some(&a)));
    assert!(!wanted(&oom("b"), Some(&a)));
    assert!(wanted(&oom("b"), None));
    // Global events (`Event::workspace()` is `None`) reach filtered subscribers too. No real
    // global variant exists yet, so the rule is checked on the workspace alone.
    assert!(wanted_for(None, Some(&a)));
    assert!(wanted_for(None, None));
}

#[test]
fn emitting_without_subscribers_is_fine() {
    let hub = EventHub::new(0);
    hub.emit(oom("a"));
    assert_eq!(hub.subscribers(), 0);
    let _rx = hub.subscribe();
    assert_eq!(hub.subscribers(), 1);
}

#[tokio::test]
async fn stream_filters_reports_lag_and_ends_on_shutdown() {
    let hub = EventHub::new(2);
    let (stop, shutdown) = watch::channel(false);
    let mut s = Box::pin(stream(hub.subscribe(), Some(name("a")), shutdown));
    hub.emit(oom("b"));
    hub.emit(Event::StatusChanged {
        workspace: name("a"),
        status: WorkspaceStatus::Running,
    });
    assert!(s.next().await.is_some(), "the status event for a");
    for _ in 0..5 {
        hub.emit(oom("a"));
    }
    // Five events into a buffer of two: the subscriber learns it missed three.
    let next = s.next().await.unwrap().unwrap();
    assert!(format!("{next:?}").contains("lagged"), "{next:?}");
    stop.send(true).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while s.next().await.is_some() {}
    })
    .await;
    assert!(end.is_ok(), "stream ended on shutdown");
}

#[tokio::test]
async fn stream_ends_when_the_hub_goes() {
    let hub = EventHub::default();
    let (_stop, shutdown) = watch::channel(false);
    let mut s = Box::pin(stream(hub.subscribe(), None, shutdown));
    drop(hub);
    assert!(s.next().await.is_none());
}
