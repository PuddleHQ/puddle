// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::clock::ManualClock;
use puddle_types::{ConnectionDecision, ConnectionReason};

fn store() -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    (clock, store)
}

#[test]
fn an_audit_record_carries_the_name_its_workspace_had_when_it_was_written() {
    let (_, store) = store();
    let acme = WorkspaceId::new("acme").unwrap();
    let op = |workspace: Option<&WorkspaceId>| NewWorkspaceOp {
        op: WorkspaceOpKind::Rename,
        workspace: workspace.cloned(),
        user: None,
        outcome: WorkspaceOpOutcome::Succeeded,
        actor: Actor::Ui,
        detail: None,
    };
    store.record_workspace_op(&op(Some(&acme))).unwrap(); // before any name
    store.set_workspace_name(&acme, "Acme shop").unwrap();
    store.record_workspace_op(&op(Some(&acme))).unwrap();
    store.set_workspace_name(&acme, "Acme").unwrap();
    store.record_workspace_op(&op(Some(&acme))).unwrap();
    store.record_workspace_op(&op(None)).unwrap(); // no workspace

    let ids: Vec<i64> = store
        .audit_lines(0, 100)
        .unwrap()
        .iter()
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(ids.len(), 4);
    let names = store.audit_workspace_names(&ids).unwrap();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(names[&ids[1]], "Acme shop");
    assert_eq!(names[&ids[2]], "Acme");
    assert!(store.audit_workspace_names(&[]).unwrap().is_empty());
}

#[test]
fn a_workspace_name_that_cannot_be_written_is_dropped_and_the_record_has_no_name() {
    let (_, store) = store();
    let ws = WorkspaceId::new("acme").unwrap();
    assert!(store.note_workspace_name(&ws, "Acme shop"));
    assert!(
        store.note_workspace_name(&ws, "Acme"),
        "a rename replaces it"
    );
    store.break_database("DROP TABLE workspace_names");
    assert!(!store.note_workspace_name(&ws, "Lost"));
    assert!(store.set_workspace_name(&ws, "Lost").is_err());
}

#[test]
fn a_write_waits_for_another_connection_holding_the_file_instead_of_failing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Store::open(&path, clock, Limits::default()).unwrap();
    // The other writer: the CLI, or the host a restart has not finished with.
    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let release = std::thread::spawn(move || {
        // The write below blocks inside the call under test, so no signal can mark the moment to
        // release: the lock is held for a fixed window that the write has to wait out.
        std::thread::sleep(Duration::from_millis(300));
        holder.execute_batch("COMMIT").unwrap();
    });
    let made = store.create_rule_set(UserId::OWNER, "Waits", "", None, Actor::Ui);
    release.join().unwrap();
    assert!(made.is_ok(), "{:?}", made.err());
}

#[test]
fn an_unreadable_set_colour_is_no_choice_and_does_not_break_the_list() {
    let (_, store) = store();
    let made = store
        .create_rule_set(UserId::OWNER, "Odd", "", None, Actor::Ui)
        .unwrap();
    for bad in ["not json", r#"{"kind":"conic"}"#] {
        lock(&store.conn)
            .execute("UPDATE rule_sets SET fill = ?1", [bad])
            .unwrap();
        let listed = store.rule_sets(UserId::OWNER).unwrap();
        let shown = listed.iter().find(|s| s.id == made.id).unwrap();
        assert_eq!(shown.fill, made.fill, "{bad}");
    }
}

#[test]
fn corrupt_rows_are_reported_not_trusted() {
    let (_, store) = store();
    {
        let conn = lock(&store.conn);
        conn.execute(
                "INSERT INTO rules (scope, workspace_id, pattern_kind, pattern, effect, created_at, created_by)
                 VALUES ('global', NULL, 'exact', 'NOT A HOST', 'allow', 0, 'cli')",
                [],
            )
            .unwrap();
    }
    let err = store
        .set_rule_expiry(RuleId(1), None, Actor::Cli)
        .unwrap_err();
    assert!(
        matches!(
            err,
            StoreError::Corrupt {
                table: "rules",
                id: 1,
                ..
            }
        ),
        "{err}"
    );
    // The bad row also blocks reloading the rule set, so no change commits on top of it.
    assert_eq!(store.rules_every_user(), Vec::<Rule>::new());
}

fn is_corrupt(err: &StoreError, in_table: &str) -> bool {
    matches!(err, StoreError::Corrupt { table, .. } if *table == in_table)
}

#[test]
fn damaged_built_in_snapshots_are_corrupt_not_empty() {
    let (clock, store) = store();
    let slug = crate::catalogue::BUILT_IN_SETS[0].slug;
    let mut conn = lock(&store.conn);
    conn_exec(
        &conn,
        "UPDATE builtin_sets_seen SET entries = 'not json' WHERE slug = ?1",
        slug,
    );
    let err = sets::note_built_in_changes(&mut conn, clock.now_ms()).unwrap_err();
    assert!(is_corrupt(&err, "builtin_sets_seen"), "{err}");
    assert!(err.to_string().contains(slug), "{err}");
    assert_eq!(
        audit_count(&conn),
        0,
        "nothing was audited as added from the damage"
    );
}

fn conn_exec(conn: &Connection, sql: &str, slug: &str) {
    conn.execute(sql, [slug]).unwrap();
}

fn audit_count(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT count(*) FROM audit WHERE type = 'rule_set_changed'",
        [],
        |r| r.get(0),
    )
    .unwrap()
}

#[test]
fn a_negative_changed_at_is_corrupt_not_never_changed() {
    let (_, store) = store();
    let slug = crate::catalogue::BUILT_IN_SETS[0].slug;
    conn_exec(
        &lock(&store.conn),
        "UPDATE builtin_sets_seen SET changed_at = -5 WHERE slug = ?1",
        slug,
    );
    let err = store.rule_sets(UserId::OWNER).unwrap_err();
    assert!(is_corrupt(&err, "builtin_sets_seen"), "{err}");
}

#[test]
fn a_negative_audit_size_is_corrupt_not_zero() {
    let (_, store) = store();
    lock(&store.conn)
        .execute("UPDATE audit_size SET bytes = -1 WHERE id = 1", [])
        .unwrap();
    let err = store.audit_bytes().unwrap_err();
    assert!(is_corrupt(&err, "audit_size"), "{err}");
}

#[test]
fn raw_rows_with_bad_fields_are_corrupt() {
    let base = || RawRule {
        id: 1,
        scope: "global".into(),
        workspace_id: None,
        pattern_kind: "exact".into(),
        pattern: "example.com".into(),
        effect: "allow".into(),
        expires_at: None,
        created_at: 0,
        created_by: "cli".into(),
        source_pending_id: None,
        set_id: None,
        user_id: 1,
    };
    assert!(rule_from_raw(&base()).is_ok());
    let cases: Vec<fn(&mut RawRule)> = vec![
        |r| r.scope = "workspace".into(),
        |r| r.workspace_id = Some("x".into()),
        |r| r.pattern_kind = "suffix".into(),
        |r| r.pattern_kind = "regex".into(),
        |r| r.effect = "maybe".into(),
        |r| r.created_by = "system".into(),
        |r| r.created_by = "guest".into(),
        |r| r.created_at = -1,
        |r| r.expires_at = Some(-5),
        |r| {
            r.pattern_kind = "suffix".into();
            r.pattern = ".10.0.0.1".into();
        },
    ];
    for (index, case) in cases.into_iter().enumerate() {
        let mut raw = base();
        case(&mut raw);
        let parsed = rule_from_raw(&raw);
        // `created_by = system` parses as an actor; the schema's CHECK keeps it out of rules.
        if index == 5 {
            continue;
        }
        assert!(parsed.is_err(), "case {index}");
    }
}

#[test]
fn raw_pending_rows_with_bad_fields_are_corrupt() {
    let base = || RawPending {
        id: 1,
        workspace_id: "sb".into(),
        host: "example.com".into(),
        port: 443,
        first_seen: 0,
        last_seen: 0,
        attempts: 1,
        state: "requested".into(),
        decided_at: None,
        decided_by: None,
        rule_id: None,
        rule_set: None,
    };
    assert!(pending_from_raw(&base()).is_ok());
    let cases: Vec<fn(&mut RawPending)> = vec![
        |r| r.workspace_id = String::new(),
        |r| r.host = "Example.com".into(),
        |r| r.port = 70_000,
        |r| r.attempts = -1,
        |r| r.state = "open".into(),
        |r| r.decided_by = Some("guest".into()),
        |r| r.decided_at = Some(-1),
    ];
    for (index, case) in cases.into_iter().enumerate() {
        let mut raw = base();
        case(&mut raw);
        assert!(pending_from_raw(&raw).is_err(), "case {index}");
    }
}

/// The counts the audit holds, summed: what the user can read in the activity log.
fn recorded_count(store: &Store, kind: &str, field: &str) -> u64 {
    store
        .audit_lines(0, 100_000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| serde_json::from_str::<serde_json::Value>(&line).unwrap())
        .filter(|v| v["type"] == kind)
        .map(|v| v[field].as_u64().unwrap())
        .sum()
}

/// The connections the `suppressed` summaries stand for.
fn summarised_connections(store: &Store) -> u64 {
    store
        .audit_lines(0, 100_000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| serde_json::from_str::<serde_json::Value>(&line).unwrap())
        .filter(|v| v["type"] == "connection" && v["reason"] == "suppressed")
        .map(|v| v["count"].as_u64().unwrap())
        .sum()
}

fn request(workspace: &str, host: &str) -> EgressRequest {
    EgressRequest::new(
        WorkspaceId::new(workspace).unwrap(),
        Host::parse_normalised(host).unwrap(),
        443,
    )
}

fn fresh_store() -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
    (clock, store)
}

#[test]
fn a_failed_sweep_flush_keeps_connection_summaries() {
    let (clock, store) = fresh_store();
    let event = ConnectionEvent::new(
        &request("a", "x.example"),
        ConnectionDecision::Allow,
        ConnectionReason::Rule,
    );
    // The default limit is 200 a second: overshoot it, then let the second pass.
    for _ in 0..205 {
        store.record_connection(&event).unwrap();
    }
    store.fail_audit_writes("connection");
    clock.advance(61_000);
    assert!(store.sweep().is_err());
    store.audit_writes_work_again();
    store.sweep().unwrap();
    assert_eq!(
        summarised_connections(&store),
        5,
        "the connections over the limit are in one summary"
    );
}

#[test]
fn a_failed_connection_summary_write_is_written_with_the_next_connection() {
    let (clock, store) = fresh_store();
    let event = ConnectionEvent::new(
        &request("a", "x.example"),
        ConnectionDecision::Allow,
        ConnectionReason::Rule,
    );
    for _ in 0..205 {
        store.record_connection(&event).unwrap();
    }
    store.fail_audit_writes("connection");
    clock.advance(1000);
    assert!(store.record_connection(&event).is_err());
    store.audit_writes_work_again();
    store.record_connection(&event).unwrap();
    assert_eq!(summarised_connections(&store), 5);
}

fn puddle_event() -> ConnectionEvent {
    ConnectionEvent::puddle(
        Host::parse_normalised("api.github.com").unwrap(),
        443,
        ConnectionDecision::Allow,
        ConnectionReason::PuddleRequest,
    )
}

#[test]
fn a_failed_sweep_flush_keeps_the_summary_of_puddles_own_connections_too() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let limits = Limits {
        connection_records_per_second: 1,
        ..Limits::default()
    };
    let store = Store::open_in_memory(clock.clone(), limits).unwrap();
    for _ in 0..4 {
        store.record_connection(&puddle_event()).unwrap();
    }
    store.fail_audit_writes("connection");
    clock.advance(2_000);
    assert!(store.sweep().is_err());
    store.audit_writes_work_again();
    store.sweep().unwrap();
    assert_eq!(summarised_connections(&store), 3);
}

#[test]
fn a_summary_is_written_alone_when_the_new_second_admits_no_record() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let limits = Limits {
        connection_records_per_second: 0,
        ..Limits::default()
    };
    let store = Store::open_in_memory(clock.clone(), limits).unwrap();
    store.record_connection(&puddle_event()).unwrap();
    clock.advance(1_000);
    store.record_connection(&puddle_event()).unwrap();
    assert_eq!(summarised_connections(&store), 1);
    assert_eq!(recorded_count(&store, "connection", "count"), 1);
}

#[test]
fn debug_does_not_dump_the_database() {
    let (_, store) = store();
    assert!(format!("{store:?}").starts_with("Store { limits"));
}

#[test]
fn timestamp_helpers_saturate() {
    assert_eq!(sql_ts(u64::MAX), i64::MAX);
    assert_eq!(plus(u64::MAX - 1, Duration::from_secs(1)), u64::MAX);
    assert!(stored_ts("rules", 1, -1).is_err());
}

#[test]
fn peeking_as_a_user_answers_from_that_users_rules_only() {
    let (_, store) = store();
    let ana = store
        .add_user(&crate::UserName::new("ana").unwrap())
        .unwrap();
    let host = Host::parse_normalised("api.example.com").unwrap();
    let elsewhere = Host::parse_normalised("other.example.com").unwrap();
    let workspace = WorkspaceId::new("not-made-yet").unwrap();
    let rule = store
        .add_rule_as(
            ana.id,
            &NewRule {
                scope: Scope::Global,
                pattern: Pattern::Exact(host.clone()),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Ui,
            },
        )
        .unwrap();

    match store.peek_as(ana.id, &workspace, &host) {
        Some(Decision::Allow { rule_id, .. }) => assert_eq!(rule_id, rule.id),
        other => panic!("expected the user's allow rule, got {other:?}"),
    }
    assert_eq!(store.peek_as(ana.id, &workspace, &elsewhere), None);
    assert_eq!(store.peek_as(UserId::OWNER, &workspace, &host), None);
}
