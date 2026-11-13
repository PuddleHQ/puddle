// SPDX-License-Identifier: AGPL-3.0-or-later
//! The events the store emits for every change, and the filtered audit queries behind
//! `GET /api/audit`.
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::*;
use puddle_store::{
    Actor, AuditCursor, AuditFilter, AuditOutcome, AuditRecord, ConnectionRecord, Effect, Limits,
    ManualClock, NewRule, Pattern, PendingExpiryReason, PendingWire, RequestRuleWire, Resolution,
    RuleDeleteReason, RuleWire, Scope, Store,
};
use puddle_test_kit::cpu_time;
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
    (clock, sink, store)
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
        .audit_query_every_user(&AuditFilter::default(), AuditCursor::Before(None), 1)
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
        .as_user(puddle_store::UserId::OWNER)
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
        .as_user(puddle_store::UserId::OWNER)
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
        .as_user(puddle_store::UserId::OWNER)
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
            .audit_query_every_user(&AuditFilter::default(), AuditCursor::After(0), 100)
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn trimming_the_audit_emits_for_its_trim_record() {
    let (_, sink, store) = fixture(Limits {
        audit_max_bytes: 400,
        audit_trim_to_bytes: 200,
        ..Limits::default()
    });
    for i in 0..6 {
        ask(&store, "a", &format!("h{i}.example"));
    }
    drop(sink.take());
    let report = store.sweep().unwrap();
    assert!(report.audit_records_trimmed > 0);
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
    ask(&store, "a", "example.com");
}

// ---------------------------------------------------------------------------------------------
// Audit queries

fn pending_wire(workspace: &str, host: &str, state: &str) -> PendingWire {
    PendingWire {
        id: 1,
        workspace_id: workspace.into(),
        host: host.into(),
        port: 443,
        first_seen: 1,
        last_seen: 1,
        attempts: 1,
        state: state.into(),
        decided_at: None,
        decided_by: None,
        rule_id: None,
        rule_set: None,
    }
}

fn rule_wire(workspace: Option<&str>, pattern: &str) -> RuleWire {
    RuleWire {
        id: 1,
        scope: workspace.map_or("global", |_| "workspace").into(),
        workspace_id: workspace.map(Into::into),
        pattern_kind: "exact".into(),
        pattern: pattern.into(),
        effect: "allow".into(),
        expires_at: None,
        created_at: 1,
        created_by: "cli".into(),
        source_pending_id: None,
        set_id: None,
    }
}

/// A connection record; an empty `workspace` is one of Puddle's own (no workspace).
fn connection(ts: u64, workspace: &str, host: &str, decision: ConnectionDecision) -> AuditRecord {
    AuditRecord::Connection(ConnectionRecord {
        ts,
        workspace_id: (!workspace.is_empty()).then(|| workspace.to_owned()),
        origin: if workspace.is_empty() {
            ConnectionOrigin::Puddle
        } else {
            ConnectionOrigin::Workspace
        },
        host: Some(host.into()),
        port: Some(443),
        resolved_ip: None,
        upstream: None,
        decision: Some(decision),
        outcome: None,
        reason: "rule".into(),
        rule_id: None,
        rule_set: None,
        request_rule: None,
        refused_address: None,
        refused_reason: None,
        category: None,
        signal: None,
        pending_id: None,
        binding_id: None,
        injected: false,
        unswapped_stand_ins: Vec::new(),
        scrubbed_stand_ins: Vec::new(),
        foreign_credentials: Vec::new(),
        answer_credentials: Vec::new(),
        method: None,
        path: None,
        path_truncated: false,
        bytes_up: 0,
        bytes_down: 0,
        count: None,
    })
}

/// `record` (a connection) as a request of `method` and `path`.
fn with_request(mut record: AuditRecord, method: &str, path: &str) -> AuditRecord {
    if let AuditRecord::Connection(c) = &mut record {
        c.method = Some(method.into());
        c.path = Some(path.into());
    }
    record
}

/// A store on disk, with `records` inserted the way `Store` inserts them (same columns).
fn seeded(records: &[AuditRecord]) -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let clock = Arc::new(ManualClock::new(T0));
    let store = Store::open(&path, clock, Limits::default()).unwrap();
    let mut conn = rusqlite::Connection::open(&path).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    let tx = conn.transaction().unwrap();
    {
        let mut insert = tx
            .prepare(
                "INSERT INTO audit (ts, type, workspace_id, host, outcome, line, search, user_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)",
            )
            .unwrap();
        for record in records {
            insert
                .execute(params![
                    i64::try_from(record.ts()).unwrap(),
                    record.kind(),
                    record.workspace_id(),
                    record.host(),
                    record.outcome().map(AuditOutcome::as_str),
                    record.to_line().unwrap(),
                    record.search_text(),
                ])
                .unwrap();
        }
    }
    tx.commit().unwrap();
    (dir, store)
}

fn naive(records: &[AuditRecord], filter: &AuditFilter) -> Vec<i64> {
    records
        .iter()
        .zip(1_i64..)
        .filter(|(r, _)| {
            filter
                .workspace
                .as_ref()
                .is_none_or(|s| r.workspace_id() == Some(s.as_str()))
                && filter.kind.is_none_or(|k| r.kind() == k)
                && filter.outcome.is_none_or(|o| r.outcome() == Some(o))
                && filter.origin.is_none_or(|o| r.origin() == Some(o))
                && filter.from.is_none_or(|t| r.ts() >= t)
                && filter.to.is_none_or(|t| r.ts() < t)
                && filter.host_contains.as_ref().is_none_or(|h| {
                    r.host()
                        .is_some_and(|host| host.contains(&h.to_lowercase()))
                })
                && filter.q.as_ref().is_none_or(|q| {
                    r.search_text()
                        .is_some_and(|text| text.contains(&q.to_ascii_lowercase()))
                })
        })
        .map(|(_, id)| id)
        .collect()
}

fn ids(rows: Vec<(i64, String)>) -> Vec<i64> {
    rows.into_iter().map(|(id, _)| id).collect()
}

fn any_record() -> impl Strategy<Value = AuditRecord> {
    let workspace = prop::sample::select(vec!["a", "b", "c"]);
    let host = prop::sample::select(vec![
        "example.com",
        "api.example.com",
        "github.com",
        "Xn--bcher-kva.example",
        "10.0.0.1",
    ]);
    let ts = (0_u64..20).prop_map(|t| T0 + t * 1000);
    let decision = prop::sample::select(vec![
        ConnectionDecision::Allow,
        ConnectionDecision::Deny,
        ConnectionDecision::Pending,
        ConnectionDecision::Blocked,
    ]);
    let state = prop::sample::select(vec!["requested", "allowed", "denied", "expired"]);
    (0..8_u8, workspace, host, ts, decision, state).prop_map(|(kind, sb_, host, ts, d, st)| {
        let host = host.to_lowercase();
        match kind {
            0 => with_request(connection(ts, sb_, &host, d), "GET", "/V1/Items"),
            1 => connection(ts, "", &host, d),
            2 => AuditRecord::PendingCreated {
                ts,
                pending: pending_wire(sb_, &host, "requested"),
            },
            3 => AuditRecord::PendingDecided {
                ts,
                pending: pending_wire(sb_, &host, st),
            },
            4 => AuditRecord::PendingExpired {
                ts,
                pending: pending_wire(sb_, &host, "expired"),
                reason: PendingExpiryReason::Stale,
            },
            5 => AuditRecord::RuleCreated {
                ts,
                rule: rule_wire(Some(sb_), &host),
            },
            6 => AuditRecord::RuleDeleted {
                ts,
                rule: rule_wire(None, &format!(".{host}")),
                reason: RuleDeleteReason::User,
                actor: "cli".into(),
            },
            _ => AuditRecord::PendingSuppressed {
                ts,
                workspace_id: sb_.into(),
                count: 3,
            },
        }
    })
}

fn any_filter() -> impl Strategy<Value = AuditFilter> {
    (
        prop::option::of(prop::sample::select(vec!["a", "b", "zzz"])),
        prop::option::of(prop::sample::select(AuditRecord::KINDS.to_vec())),
        prop::option::of(prop::sample::select(AuditOutcome::ALL.to_vec())),
        prop::option::of(prop::sample::select(vec![
            ConnectionOrigin::Workspace,
            ConnectionOrigin::Puddle,
        ])),
        prop::option::of(prop::sample::select(vec![
            "example",
            "EXAMPLE.com",
            ".example",
            "10.0",
            "nothing",
            "git",
        ])),
        prop::option::of(prop::sample::select(vec![
            "EXAMPLE", "get", "/v1", "rule", "nothing",
        ])),
        prop::option::of(0_u64..22),
        prop::option::of(0_u64..22),
    )
        .prop_map(
            |(workspace, kind, outcome, origin, host, q, from, to)| AuditFilter {
                user: None,
                workspace: workspace.map(sb),
                kind,
                outcome,
                origin,
                host_contains: host.map(str::to_owned),
                q: q.map(str::to_owned),
                category: None,
                from: from.map(|t| T0 + t * 1000),
                to: to.map(|t| T0 + t * 1000),
            },
        )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Every filter combination returns exactly what filtering the records by hand returns,
    /// in both directions, and paging back with `Before` visits each match once.
    #[test]
    fn filtered_queries_equal_a_naive_filter(
        records in prop::collection::vec(any_record(), 0..60),
        filter in any_filter(),
        page in 1_u32..9,
    ) {
        let (_dir, store) = seeded(&records);
        let want = naive(&records, &filter);
        let up = ids(store.audit_query_every_user(&filter, AuditCursor::After(0), 1000).unwrap());
        prop_assert_eq!(&up, &want);
        let mut down = ids(store.audit_query_every_user(&filter, AuditCursor::Before(None), 1000).unwrap());
        down.reverse();
        prop_assert_eq!(&down, &want);

        let mut paged = Vec::new();
        let mut cursor = AuditCursor::Before(None);
        loop {
            let rows = ids(store.audit_query_every_user(&filter, cursor, page).unwrap());
            let Some(last) = rows.last().copied() else { break };
            prop_assert!(rows.len() <= page as usize);
            paged.extend(rows);
            cursor = AuditCursor::Before(Some(last));
        }
        paged.reverse();
        prop_assert_eq!(paged, want);
    }
}

#[test]
fn filters_combine_and_the_host_is_case_folded() {
    let records = vec![
        connection(T0, "a", "api.example.com", ConnectionDecision::Allow),
        connection(T0 + 1, "a", "api.example.com", ConnectionDecision::Deny),
        connection(T0 + 2, "b", "api.example.com", ConnectionDecision::Allow),
        AuditRecord::RuleCreated {
            ts: T0 + 3,
            rule: rule_wire(Some("a"), "api.example.com"),
        },
    ];
    let (_dir, store) = seeded(&records);
    let q = |filter: AuditFilter| {
        ids(store
            .audit_query_every_user(&filter, AuditCursor::After(0), 100)
            .unwrap())
    };
    assert_eq!(q(AuditFilter::default()), [1, 2, 3, 4]);
    assert_eq!(
        q(AuditFilter {
            workspace: Some(sb("a")),
            kind: Some("connection"),
            outcome: Some(AuditOutcome::Allow),
            host_contains: Some("API.EXAMPLE".into()),
            ..AuditFilter::default()
        }),
        [1]
    );
    // A rule matches on its pattern, and has no outcome.
    assert_eq!(
        q(AuditFilter {
            host_contains: Some("example".into()),
            kind: Some("rule_created"),
            ..AuditFilter::default()
        }),
        [4]
    );
    assert_eq!(
        q(AuditFilter {
            outcome: Some(AuditOutcome::Expired),
            ..AuditFilter::default()
        }),
        Vec::<i64>::new()
    );
    // `from` is inclusive and `to` exclusive.
    assert_eq!(
        q(AuditFilter {
            from: Some(T0 + 1),
            to: Some(T0 + 3),
            ..AuditFilter::default()
        }),
        [2, 3]
    );
}

#[test]
fn the_category_filter_matches_company_network_connections_only() {
    let mut company = connection(T0, "a", "nexus.corp.example", ConnectionDecision::Allow);
    let mut refused = connection(T0 + 1, "a", "10.8.0.9", ConnectionDecision::Deny);
    let mut lookalike = connection(
        T0 + 2,
        "a",
        "company_network.example",
        ConnectionDecision::Allow,
    );
    for (record, reason) in [
        (&mut company, "rule"),
        (&mut refused, "toggle:company_network"),
    ] {
        if let AuditRecord::Connection(c) = record {
            c.reason = reason.into();
            c.category = Some("company_network".into());
        }
    }
    if let AuditRecord::Connection(c) = &mut lookalike {
        c.reason = "toggle:company_network".into();
    }
    let records = vec![
        company,
        refused,
        lookalike,
        connection(T0 + 3, "a", "api.example.com", ConnectionDecision::Allow),
    ];
    let (_dir, store) = seeded(&records);
    let found = |category: Option<&'static str>| {
        ids(store
            .audit_query_every_user(
                &AuditFilter {
                    category,
                    ..AuditFilter::default()
                },
                AuditCursor::After(0),
                100,
            )
            .unwrap())
    };
    assert_eq!(found(Some("company_network")), [1, 2]);
    assert_eq!(found(None), [1, 2, 3, 4]);
}

#[test]
fn the_search_box_matches_each_text_column_and_combines_with_the_other_filters() {
    let mut denied = connection(T0 + 1, "b", "evil.example.org", ConnectionDecision::Deny);
    if let AuditRecord::Connection(c) = &mut denied {
        c.reason = "request_rule".into();
    }
    let records = vec![
        // 1: host, method and path.
        with_request(
            connection(T0, "a", "api.example.com", ConnectionDecision::Allow),
            "POST",
            "/Orders/42",
        ),
        // 2: the reason.
        denied,
        // 3: a rule's pattern.
        AuditRecord::RuleCreated {
            ts: T0 + 2,
            rule: rule_wire(Some("a"), ".Corp.Internal"),
        },
        // 4: a pending request's host.
        AuditRecord::PendingCreated {
            ts: T0 + 3,
            pending: pending_wire("a", "queued.example.net", "requested"),
        },
        // 5: a rule on method and path, by its host and path.
        AuditRecord::RequestRuleCreated {
            ts: T0 + 4,
            rule: RequestRuleWire {
                id: 1,
                scope: "global".into(),
                workspace_id: None,
                host: "files.example.io".into(),
                methods: vec!["DELETE".into()],
                path: "/Admin/**".into(),
                created_at: T0,
                created_by: "ui".into(),
            },
        },
        // 6: no searchable text at all.
        AuditRecord::PendingSuppressed {
            ts: T0 + 5,
            workspace_id: "a".into(),
            count: 2,
        },
    ];
    let (_dir, store) = seeded(&records);
    let found = |filter: AuditFilter| {
        ids(store
            .audit_query_every_user(&filter, AuditCursor::After(0), 100)
            .unwrap())
    };
    let q = |text: &str| {
        found(AuditFilter {
            q: Some(text.into()),
            ..AuditFilter::default()
        })
    };
    assert_eq!(q("api.example"), [1], "host");
    assert_eq!(q("POST"), [1], "method, case-folded");
    assert_eq!(q("orders/42"), [1], "path, case-folded");
    assert_eq!(q("request_rule"), [2], "reason");
    assert_eq!(q("corp.internal"), [3], "rule pattern");
    assert_eq!(q("queued"), [4], "pending host");
    assert_eq!(q("files.example.io"), [5], "method and path rule host");
    assert_eq!(q("/admin/"), [5], "method and path rule path");
    assert_eq!(q("EXAMPLE"), [1, 2, 4, 5], "several columns, several kinds");
    assert_eq!(q("nothing-like-it"), Vec::<i64>::new());
    // Text that spans two fields is no hit (the API refuses a line break in `q`, the fields'
    // separator).
    assert_eq!(q("com post"), Vec::<i64>::new());
    // Every other filter narrows the search, and `q` narrows them.
    let with = |q: &str, filter: AuditFilter| {
        found(AuditFilter {
            q: Some(q.into()),
            ..filter
        })
    };
    assert_eq!(
        with(
            "example",
            AuditFilter {
                workspace: Some(sb("a")),
                ..AuditFilter::default()
            }
        ),
        [1, 4]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                kind: Some("connection"),
                ..AuditFilter::default()
            }
        ),
        [1, 2]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                outcome: Some(AuditOutcome::Deny),
                ..AuditFilter::default()
            }
        ),
        [2]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                origin: Some(ConnectionOrigin::Workspace),
                ..AuditFilter::default()
            }
        ),
        [1, 2]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                from: Some(T0 + 1),
                to: Some(T0 + 4),
                ..AuditFilter::default()
            }
        ),
        [2, 4]
    );
    assert_eq!(
        with(
            "example",
            AuditFilter {
                host_contains: Some("evil".into()),
                ..AuditFilter::default()
            }
        ),
        [2]
    );
    assert_eq!(
        with(
            "orders",
            AuditFilter {
                workspace: Some(sb("b")),
                ..AuditFilter::default()
            }
        ),
        Vec::<i64>::new()
    );
}

#[test]
fn hostile_filter_text_is_data_not_sql_or_a_pattern() {
    let (_dir, store) = seeded(&[connection(
        T0,
        "a",
        "example.com",
        ConnectionDecision::Allow,
    )]);
    for needle in ["%", "_", "' OR 1=1 --", "\"; DROP TABLE audit; --", "\u{0}"] {
        let rows = store
            .audit_query_every_user(
                &AuditFilter {
                    host_contains: Some(needle.into()),
                    ..AuditFilter::default()
                },
                AuditCursor::After(0),
                10,
            )
            .unwrap();
        assert!(rows.is_empty(), "{needle:?}");
        let rows = store
            .audit_query_every_user(
                &AuditFilter {
                    q: Some(needle.into()),
                    ..AuditFilter::default()
                },
                AuditCursor::After(0),
                10,
            )
            .unwrap();
        assert!(rows.is_empty(), "q {needle:?}");
    }
    assert_eq!(
        store
            .audit_query_every_user(&AuditFilter::default(), AuditCursor::After(0), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
#[expect(clippy::too_many_lines, reason = "one table of query cases")]
fn a_hundred_thousand_records_answer_every_filter_quickly() {
    const ROWS: u64 = 100_000;
    let workspaces = ["alpha", "beta", "gamma", "delta"];
    let decisions = [
        ConnectionDecision::Allow,
        ConnectionDecision::Deny,
        ConnectionDecision::Pending,
        ConnectionDecision::Blocked,
    ];
    let records: Vec<AuditRecord> = (0..ROWS)
        .map(|i| {
            let workspace = workspaces[(i % 4) as usize];
            let ts = T0 + i * 10;
            match i % 50 {
                0 => AuditRecord::RuleCreated {
                    ts,
                    rule: rule_wire(Some(workspace), &format!("rule{i}.example.com")),
                },
                1 => AuditRecord::PendingCreated {
                    ts,
                    pending: pending_wire(workspace, &format!("host{i}.example.org"), "requested"),
                },
                _ => with_request(
                    connection(
                        ts,
                        workspace,
                        &format!("h{}.site{}.example.net", i % 97, i % 13),
                        decisions[(i % 4) as usize],
                    ),
                    "GET",
                    &format!("/v1/items/{i}/details"),
                ),
            }
        })
        .collect();
    let (_dir, store) = seeded(&records);
    let last = T0 + ROWS * 10;
    // Each case names the plan step it must use. The wall clock below is only a backstop: the
    // plan is what proves a filter doesn't fall back to scanning the whole table, and it
    // doesn't depend on how busy the machine is.
    let cases: Vec<(&str, AuditFilter, AuditCursor, &str)> = vec![
        (
            "newest page",
            AuditFilter::default(),
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "older page",
            AuditFilter::default(),
            AuditCursor::Before(Some(50_000)),
            "USING INTEGER PRIMARY KEY (rowid<?)",
        ),
        (
            "workspace",
            AuditFilter {
                workspace: Some(sb("beta")),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_workspace (workspace_id=?)",
        ),
        (
            "type",
            AuditFilter {
                kind: Some("pending_created"),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_type (type=?)",
        ),
        (
            "outcome",
            AuditFilter {
                outcome: Some(AuditOutcome::Blocked),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_outcome (outcome=?)",
        ),
        (
            "last hour (all of it)",
            AuditFilter {
                from: Some(last - 3_600_000),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "narrow time range",
            AuditFilter {
                from: Some(T0 + 400_000),
                to: Some(T0 + 410_000),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_ts (ts>? AND ts<?)",
        ),
        (
            "host, common",
            AuditFilter {
                host_contains: Some("EXAMPLE.net".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "host, rare (scans everything)",
            AuditFilter {
                host_contains: Some("rule99950".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "host, absent (scans everything)",
            AuditFilter {
                host_contains: Some("absent.invalid".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, common (host, method and path text)",
            AuditFilter {
                q: Some("EXAMPLE.net".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, common word in the path",
            AuditFilter {
                q: Some("details".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, rare (a path near the oldest record: scans everything)",
            AuditFilter {
                q: Some("/v1/items/52/details".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search, absent (scans everything)",
            AuditFilter {
                q: Some("absent.invalid".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "SCAN audit",
        ),
        (
            "search with a workspace and a type",
            AuditFilter {
                q: Some("absent.invalid".into()),
                workspace: Some(sb("beta")),
                kind: Some("connection"),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            "USING INDEX audit_workspace (workspace_id=?)",
        ),
        (
            "everything at once",
            AuditFilter {
                user: Some(puddle_store::UserId::OWNER),
                workspace: Some(sb("gamma")),
                kind: Some("connection"),
                outcome: Some(AuditOutcome::Pending),
                origin: Some(ConnectionOrigin::Workspace),
                q: Some("details".into()),
                host_contains: Some("site7".into()),
                category: None,
                from: Some(T0),
                to: Some(last),
            },
            AuditCursor::Before(None),
            "USING INDEX audit_outcome (outcome=?)",
        ),
        (
            "tail",
            AuditFilter::default(),
            AuditCursor::After(99_900),
            "USING INTEGER PRIMARY KEY (rowid>?)",
        ),
    ];
    for (name, filter, cursor, step) in cases {
        let plan = store.audit_query_plan(&filter, cursor, 100).unwrap();
        assert!(
            plan.iter().any(|line| line.contains(step)),
            "{name}: expected `{step}` in the plan, got {plan:?}"
        );
        // Best of three: thread CPU time still inflates on a shared 2-vCPU runner when another
        // job bursts on the same host (CI run 42824928536: the full scans took 44 ms, then 203 ms,
        // then 501 ms within one second, against 17 ms on a free machine). A burst that long does
        // not cover three runs in a row, and a quadratic query is slow in all three.
        let mut best = Duration::MAX;
        let mut rows = Vec::new();
        for _ in 0..3 {
            let (got, cpu) =
                cpu_time(|| store.audit_query_every_user(&filter, cursor, 100).unwrap());
            best = best.min(cpu);
            rows = got;
        }
        let cpu = best;
        assert!(rows.len() <= 100);
        eprintln!("audit query {name}: {cpu:?} of CPU");
        // The bar is on this thread's CPU time, so a loaded machine barely moves it; it catches a
        // query that went quadratic (a per-row rescan costs seconds). The slowest case (a full
        // scan) used 17 to 44 ms on Linux and 47 ms on Windows (3 ticks), the indexed ones 0.1 ms.
        assert!(
            cpu < Duration::from_millis(250),
            "{name} used {cpu:?} of CPU (best of three)"
        );
    }
    // The filtered results are right, not just fast.
    let one = store
        .audit_query_every_user(
            &AuditFilter {
                host_contains: Some("rule99950".into()),
                ..AuditFilter::default()
            },
            AuditCursor::Before(None),
            100,
        )
        .unwrap();
    assert_eq!(one.len(), 1);
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
            puddle_store::UserId::OWNER,
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
