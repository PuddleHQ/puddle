// SPDX-License-Identifier: AGPL-3.0-or-later
//! The events the store emits for every change. The filtered audit queries behind
//! `GET /api/audit` are in `query`.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::*;
use puddle_store::testing::own;
use puddle_store::{
    Actor, ActorWire, AuditCursor, AuditFilter, AuditOutcome, AuditRecord, ConnectionRecord,
    Effect, Limits, ManualClock, NewRule, Pattern, PendingExpiryReason, PendingState, PendingWire,
    RequestRuleWire, RequestRuleWireScope, Resolution, RuleDeleteReason, RuleWire,
    RuleWirePatternKind, RuleWireScope, Scope, Store, UserId, UserName,
};
use puddle_types::{
    CollectingSink, ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionOrigin,
    ConnectionReason, Decision, EgressRequest, Event, Host, PendingEnd, PendingId, PendingOutcome,
    SuffixAllows, WorkspaceId,
};
use rusqlite::params;

const T0: u64 = 1_800_000_000_000;

fn sb(id: &str) -> WorkspaceId {
    WorkspaceId::new(id).unwrap()
}

fn fixture(limits: Limits) -> (Arc<ManualClock>, Arc<CollectingSink>, Store) {
    let clock = Arc::new(ManualClock::new(T0));
    let sink = Arc::new(CollectingSink::default());
    let store = Store::open_in_memory(clock.clone(), limits)
        .unwrap()
        .with_events(sink.clone());
    own_workspaces(&store);
    (clock, sink, store)
}

/// The workspaces these tests use, each the owner's.
fn own_workspaces(store: &Store) {
    for name in ["a", "b", "beta", "gamma"] {
        own(store, &sb(name));
    }
}

fn ask(store: &Store, workspace: &str, host: &str) -> PendingOutcome {
    match store
        .decide(
            &EgressRequest::new(sb(workspace), Host::parse_normalised(host).unwrap(), 443),
            SuffixAllows::Count,
        )
        .unwrap()
    {
        Decision::Pending(outcome) => outcome,
        other => panic!("expected pending, got {other:?}"),
    }
}

fn new_id(outcome: PendingOutcome) -> PendingId {
    match outcome {
        PendingOutcome::New(id) => id,
        other => panic!("expected a new row, got {other:?}"),
    }
}

fn newest_audit_id(store: &Store) -> i64 {
    store
        .audit_query_every_user(
            &puddle_store::testing::all_users(),
            &AuditFilter::default(),
            AuditCursor::Before(None),
            1,
        )
        .unwrap()
        .first()
        .map_or(0, |(id, _)| *id)
}

fn kinds(events: &[Event]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match e {
            Event::PendingOpened { .. } => "pending_opened",
            Event::PendingUpdated { .. } => "pending_updated",
            Event::PendingClosed { .. } => "pending_closed",
            Event::RulesChanged {} => "rules_changed",
            Event::AuditAppended { .. } => "audit_appended",
            _ => "other",
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Events

/// A record of a workspace nobody owns is nobody's: no user reads it, the every-user read still
/// sees it. A record that names no workspace stays the install owner's.
#[test]
fn a_record_of_a_workspace_nobody_owns_is_nobodys() {
    let (_, _, store) = fixture(Limits::default());
    let ghost = EgressRequest::new(
        sb("ghost"),
        Host::parse_normalised("example.com").unwrap(),
        443,
    );
    store.record(&ConnectionEvent::new(
        &ghost,
        ConnectionDecision::Allow,
        ConnectionReason::Rule,
    ));
    let none = |user| {
        store
            .audit_query(user, &AuditFilter::default(), AuditCursor::After(0), 10)
            .unwrap()
            .len()
    };
    assert_eq!(none(UserId::OWNER), 0);
    assert_eq!(none(UserId::from_stored(0)), 1);
    let every = store
        .audit_query_every_user(
            &puddle_store::testing::all_users(),
            &AuditFilter::default(),
            AuditCursor::After(0),
            10,
        )
        .unwrap();
    assert_eq!(every.len(), 1);
}

#[test]
fn a_new_request_opens_a_pending_row_and_appends_to_the_audit() {
    let (_, sink, store) = fixture(Limits::default());
    let id = new_id(ask(&store, "a", "api.example.co.uk"));
    let events = sink.take();
    assert_eq!(kinds(&events), ["pending_opened", "audit_appended"]);
    let Event::PendingOpened { request } = &events[0] else {
        panic!()
    };
    assert_eq!(
        (
            request.id,
            request.workspace.as_str(),
            request.host.as_str()
        ),
        (id.0, "a", "api.example.co.uk")
    );
    assert_eq!(request.registrable_domain, "example.co.uk");
    assert_eq!(
        (request.port, request.attempts, request.first_seen),
        (443, 1, T0)
    );
    assert_eq!(
        events[1],
        Event::AuditAppended {
            id: newest_audit_id(&store),
            user: 1,
        }
    );
}

#[test]
fn a_repeat_updates_the_row_and_writes_no_audit_record() {
    let (clock, sink, store) = fixture(Limits::default());
    let id = new_id(ask(&store, "a", "example.com"));
    drop(sink.take());
    clock.advance(5000);
    ask(&store, "a", "example.com");
    assert_eq!(
        sink.take(),
        [Event::PendingUpdated {
            workspace: sb("a"),
            id: id.0,
            attempts: 2,
            last_seen: T0 + 5000,
        }]
    );
}

#[test]
fn approving_closes_the_row_and_reports_the_rule_and_every_other_row_it_decides() {
    let (_, sink, store) = fixture(Limits::default());
    let first = new_id(ask(&store, "a", "api.example.com"));
    let second = new_id(ask(&store, "a", "www.example.com"));
    let other = new_id(ask(&store, "b", "www.example.com"));
    drop(sink.take());
    let mut resolution = Resolution::allow();
    resolution.pattern = puddle_store::PatternChoice::Suffix("example.com".into());
    // The CLI path emits like the API path: events come from the store, not the caller.
    let decided = store
        .resolve_pending(first, &resolution, Actor::Cli)
        .unwrap();
    assert_eq!(decided.also_closed, [second]);
    let events = sink.take();
    let closed: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            Event::PendingClosed {
                workspace,
                id,
                state,
                rule_id,
            } => Some((workspace.to_string(), *id, *state, *rule_id)),
            _ => None,
        })
        .collect();
    let rule = Some(decided.rule.id.0);
    assert_eq!(
        closed,
        [
            ("a".to_owned(), first.0, PendingEnd::Allowed, rule),
            ("a".to_owned(), second.0, PendingEnd::Allowed, rule),
        ],
        "the other workspace's row ({other:?}) is not covered by a workspace rule"
    );
    let tail = kinds(&events);
    assert_eq!(
        &tail[tail.len() - 2..],
        ["rules_changed", "audit_appended"],
        "{tail:?}"
    );
    assert_eq!(
        events.last(),
        Some(&Event::AuditAppended {
            id: newest_audit_id(&store),
            user: 1,
        })
    );
}

#[test]
fn denying_reports_denied() {
    let (_, sink, store) = fixture(Limits::default());
    let id = new_id(ask(&store, "a", "bad.example"));
    drop(sink.take());
    store
        .resolve_pending(id, &Resolution::deny(), Actor::Api)
        .unwrap();
    assert!(sink.take().iter().any(|e| matches!(
        e,
        Event::PendingClosed {
            state: PendingEnd::Denied,
            ..
        }
    )));
}

#[test]
fn rule_changes_emit_rules_changed_and_a_rule_that_decides_open_rows_closes_them() {
    let (clock, sink, store) = fixture(Limits::default());
    let id = new_id(ask(&store, "a", "www.example.com"));
    drop(sink.take());
    let rule = store
        .as_user(UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Global,
            pattern: Pattern::parse(".example.com").unwrap(),
            effect: Effect::Deny,
            expires_at: Some(T0 + 60_000),
            created_by: Actor::Cli,
        })
        .unwrap();
    let events = sink.take();
    assert!(events.contains(&Event::PendingClosed {
        workspace: sb("a"),
        id: id.0,
        state: PendingEnd::Denied,
        rule_id: Some(rule.id.0),
    }));
    assert_eq!(kinds(&events).last(), Some(&"audit_appended"));
    assert!(events.contains(&Event::RulesChanged {}));

    store.set_rule_expiry(rule.id, None, Actor::Cli).unwrap();
    assert_eq!(kinds(&sink.take()), ["rules_changed", "audit_appended"]);
    store.delete_rule(rule.id, Actor::Cli).unwrap();
    assert_eq!(kinds(&sink.take()), ["rules_changed", "audit_appended"]);

    // The sweeper's expiry of a rule is a rule change too.
    store
        .as_user(UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Global,
            pattern: Pattern::parse("short.example").unwrap(),
            effect: Effect::Allow,
            expires_at: Some(T0 + 1000),
            created_by: Actor::Cli,
        })
        .unwrap();
    drop(sink.take());
    clock.advance(2000);
    store.sweep().unwrap();
    assert_eq!(kinds(&sink.take()), ["rules_changed", "audit_appended"]);
}

#[test]
fn a_change_that_fails_emits_nothing() {
    let (_, sink, store) = fixture(Limits::default());
    let id = new_id(ask(&store, "a", "example.com"));
    store
        .resolve_pending(id, &Resolution::allow(), Actor::Cli)
        .unwrap();
    drop(sink.take());
    store
        .resolve_pending(id, &Resolution::allow(), Actor::Cli)
        .unwrap_err();
    store
        .delete_rule(puddle_types::RuleId(999), Actor::Cli)
        .unwrap_err();
    assert_eq!(sink.take(), []);
}

#[test]
fn deleted_workspaces_close_their_rows_as_expired() {
    let (_, sink, store) = fixture(Limits::default());
    let gone = new_id(ask(&store, "b", "gone.example"));
    drop(sink.take());
    store.delete_workspace(&sb("b")).unwrap();
    let events = sink.take();
    assert!(events.contains(&Event::PendingClosed {
        workspace: sb("b"),
        id: gone.0,
        state: PendingEnd::Expired,
        rule_id: None,
    }));
    // Without rules, no rules_changed.
    assert!(!events.contains(&Event::RulesChanged {}));
}

#[test]
fn deleting_a_workspace_with_rules_reports_rules_changed() {
    let (_, sink, store) = fixture(Limits::default());
    store
        .as_user(UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Workspace(sb("a")),
            pattern: Pattern::parse("example.com").unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    drop(sink.take());
    store.delete_workspace(&sb("a")).unwrap();
    assert!(sink.take().contains(&Event::RulesChanged {}));
}

#[test]
fn an_unlisted_request_opens_no_row_and_sends_no_event() {
    let limits = Limits {
        max_open_rows: 1,
        ..Limits::default()
    };
    let (_, sink, store) = fixture(limits);
    ask(&store, "a", "one.example");
    drop(sink.take());
    assert!(matches!(
        ask(&store, "a", "two.example"),
        PendingOutcome::Unlisted
    ));
    assert_eq!(sink.take(), []);
}

#[test]
fn connection_records_and_flushed_summaries_emit_one_audit_event_per_commit() {
    let (clock, sink, store) = fixture(Limits {
        connection_records_per_second: 1,
        ..Limits::default()
    });
    let request = EgressRequest::new(sb("a"), Host::parse_normalised("example.com").unwrap(), 443);
    let event = ConnectionEvent::new(&request, ConnectionDecision::Allow, ConnectionReason::Rule);
    store.record(&event);
    assert_eq!(
        sink.take(),
        [Event::AuditAppended {
            id: newest_audit_id(&store),
            user: 1,
        }]
    );
    // Over the per-second limit: counted, nothing written, nothing emitted.
    store.record(&event);
    assert_eq!(sink.take(), []);
    // The next second writes the summary and the new record in one commit: one event.
    clock.advance(1000);
    store.record(&event);
    assert_eq!(
        sink.take(),
        [Event::AuditAppended {
            id: newest_audit_id(&store),
            user: 1,
        }]
    );
    assert_eq!(
        store
            .audit_query_every_user(
                &puddle_store::testing::all_users(),
                &AuditFilter::default(),
                AuditCursor::After(0),
                100
            )
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn pruning_the_audit_emits_for_its_trim_record() {
    let (clock, sink, store) = fixture(Limits::default());
    store.set_audit_retention_days(1).unwrap();
    for i in 0..6 {
        ask(&store, "a", &format!("h{i}.example"));
    }
    clock.advance(2 * 24 * 60 * 60 * 1000);
    drop(sink.take());
    let report = store.sweep().unwrap();
    assert!(report.audit_records_pruned > 0);
    assert_eq!(
        sink.take().last(),
        Some(&Event::AuditAppended {
            id: newest_audit_id(&store),
            user: 1,
        })
    );
}

#[test]
fn a_store_without_a_sink_works() {
    let clock = Arc::new(ManualClock::new(T0));
    let store = Store::open_in_memory(clock, Limits::default()).unwrap();
    own_workspaces(&store);
    ask(&store, "a", "example.com");
}

fn rules_changed_by_deleting(store: &Store, sink: &CollectingSink, workspace: &str) -> bool {
    drop(sink.take());
    store.delete_workspace(&sb(workspace)).unwrap();
    sink.take().contains(&Event::RulesChanged {})
}

#[test]
fn deleting_a_workspace_that_only_had_a_rule_set_switch_reports_rules_changed() {
    let (_, sink, store) = fixture(Limits::default());
    store
        .switch_rule_set(
            UserId::OWNER,
            puddle_types::RuleSetId::BuiltIn("github"),
            Some(&sb("a")),
            Some(false),
            Actor::Ui,
        )
        .unwrap();
    assert!(rules_changed_by_deleting(&store, &sink, "a"));
}

#[test]
fn deleting_a_workspace_that_only_had_a_system_reason_reports_rules_changed() {
    let (_, sink, store) = fixture(Limits::default());
    let plan = puddle_store::SystemPlan {
        workspaces: [(
            sb("a"),
            [puddle_store::SystemReason::DirectSsh]
                .into_iter()
                .collect(),
        )]
        .into_iter()
        .collect(),
        ..puddle_store::SystemPlan::default()
    };
    store.set_system_managed(&plan).unwrap();
    assert!(rules_changed_by_deleting(&store, &sink, "a"));
    assert!(
        !rules_changed_by_deleting(&store, &sink, "b"),
        "nothing to drop, nothing reported"
    );
}

mod query;
