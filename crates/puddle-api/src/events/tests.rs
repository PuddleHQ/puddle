// SPDX-License-Identifier: GPL-3.0-or-later
use std::time::Duration;

use puddle_store::{Limits, ManualClock, UserName};
use puddle_types::WorkspaceStatus;

use super::*;
use crate::authz::Via;

fn store() -> Arc<Store> {
    Arc::new(Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap())
}

fn owner(store: &Arc<Store>) -> Audience {
    Audience::new(Principal::INSTALL, store.clone())
}

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
    let store = store();
    store
        .set_workspace_owner(&name("a"), UserId::OWNER)
        .unwrap();
    let mut s = Box::pin(stream(
        hub.subscribe(),
        owner(&store),
        Some(name("a")),
        shutdown,
        std::future::pending(),
    ));
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
    let mut s = Box::pin(stream(
        hub.subscribe(),
        owner(&store()),
        None,
        shutdown,
        std::future::pending(),
    ));
    drop(hub);
    assert!(s.next().await.is_none());
}

/// A user's stream carries their own workspaces' events and nothing else; the owner's stream
/// carries the install's own events and the owner's workspaces', never another user's.
#[tokio::test]
async fn each_stream_carries_only_its_principals_events() {
    let store = store();
    let ann = store.add_user(&UserName::new("ann").unwrap()).unwrap().id;
    store.set_workspace_owner(&name("anns"), ann).unwrap();
    store
        .set_workspace_owner(&name("owners"), UserId::OWNER)
        .unwrap();
    let hub = EventHub::default();
    let (_stop, shutdown) = watch::channel(false);
    let as_ann = Audience::new(
        Principal {
            user: ann,
            role: Role::User,
            via: Via::Install,
        },
        store.clone(),
    );
    let mut anns = Box::pin(stream(
        hub.subscribe(),
        as_ann,
        None,
        shutdown.clone(),
        std::future::pending(),
    ));
    let mut owners = Box::pin(stream(
        hub.subscribe(),
        owner(&store),
        None,
        shutdown,
        std::future::pending(),
    ));
    hub.emit(Event::AuditAppended { id: 7, user: 1 });
    hub.emit(oom("owners"));
    hub.emit(oom("anns"));
    // Twice, so the second comes from the cache of who owns what.
    hub.emit(oom("anns"));
    let text = |e: Option<Result<sse::Event, Infallible>>| format!("{:?}", e.unwrap().unwrap());
    for _ in 0..2 {
        assert!(text(anns.next().await).contains("anns"));
    }
    assert!(text(owners.next().await).contains("audit_appended"));
    assert!(text(owners.next().await).contains("owners"));
    // A workspace with no recorded user is nobody's: its events reach no one.
    hub.emit(oom("nobodys"));
    hub.emit(oom("owners"));
    hub.emit(Event::AuditAppended { id: 8, user: 1 });
    hub.emit(Event::RulesChanged {});
    // A signal that carries nothing reaches everyone.
    assert!(
        text(anns.next().await).contains("rules_changed"),
        "skipped the owner's workspace event and the audit"
    );
    assert!(text(owners.next().await).contains("owners"));
    assert!(text(owners.next().await).contains("audit_appended"));
    assert!(text(owners.next().await).contains("rules_changed"));
    // Ann's audit records reach Ann alone; her identities and environment signals reach her too.
    hub.emit(Event::AuditAppended {
        id: 9,
        user: ann.get(),
    });
    for signal in [
        Event::IdentitiesChanged {},
        Event::GlobalEnvChanged {},
        Event::RepoListsChanged {},
    ] {
        hub.emit(signal);
    }
    hub.emit(Event::NetworkChanged { epoch: 1 });
    hub.emit(Event::RulesChanged {});
    let anns_own = text(anns.next().await);
    assert!(
        anns_own.contains("audit_appended") && anns_own.contains('9'),
        "{anns_own}"
    );
    for kind in [
        "identities_changed",
        "global_env_changed",
        "repo_lists_changed",
        "rules_changed",
    ] {
        assert!(text(anns.next().await).contains(kind), "{kind}");
    }
    for kind in [
        "identities_changed",
        "global_env_changed",
        "repo_lists_changed",
        "network_changed",
    ] {
        assert!(text(owners.next().await).contains(kind), "{kind}");
    }
}

#[tokio::test]
async fn stream_ends_when_its_device_is_signed_out() {
    let hub = EventHub::default();
    let (_stop, shutdown) = watch::channel(false);
    let (signed_out, ended) = tokio::sync::oneshot::channel::<()>();
    let mut s = Box::pin(stream(
        hub.subscribe(),
        owner(&store()),
        None,
        shutdown,
        async move {
            let _ = ended.await;
        },
    ));
    hub.emit(Event::RulesChanged {});
    assert!(s.next().await.is_some(), "events flow until then");
    signed_out.send(()).unwrap();
    let end = tokio::time::timeout(Duration::from_secs(5), async {
        while s.next().await.is_some() {}
    })
    .await;
    assert!(end.is_ok(), "stream ended when the device was revoked");
}
