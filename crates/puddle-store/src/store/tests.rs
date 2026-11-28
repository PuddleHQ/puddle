// SPDX-License-Identifier: AGPL-3.0-or-later
use std::time::Duration;

use puddle_types::{ConnectionEvent, Host};

use super::rows::{RawPending, RawRule, pending_from_raw, rule_from_raw};
use super::*;
use crate::clock::ManualClock;
use crate::rule::Effect;
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
    store.set_workspace_owner(&acme, UserId::OWNER).unwrap();
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
    let names = store.audit_workspace_names(UserId::OWNER, &ids).unwrap();
    assert_eq!(names.len(), 2, "{names:?}");
    assert_eq!(names[&ids[1]], "Acme shop");
    assert_eq!(names[&ids[2]], "Acme");
    assert!(
        store
            .audit_workspace_names(UserId::OWNER, &[])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_policy_reports_the_current_name_of_a_workspace() {
    let (_, store) = store();
    let acme = WorkspaceId::new("acme").unwrap();
    assert_eq!(Policy::workspace_name(&store, &acme), None);
    store.set_workspace_name(&acme, "Acme shop").unwrap();
    assert_eq!(
        Policy::workspace_name(&store, &acme).as_deref(),
        Some("Acme shop")
    );
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
    assert_eq!(store.current_workspace_name(&ws).as_deref(), Some("Acme"));
    store.break_database("DROP TABLE workspace_names");
    assert!(!store.note_workspace_name(&ws, "Lost"));
    assert!(store.set_workspace_name(&ws, "Lost").is_err());
    assert_eq!(
        store.current_workspace_name(&ws),
        None,
        "an unreadable name is none"
    );
}

/// Set by the busy handler of `a_write_waits_for_another_connection_holding_the_file`.
static WRITER_IS_WAITING: AtomicBool = AtomicBool::new(false);

fn note_writer_is_waiting(_attempts: i32) -> bool {
    WRITER_IS_WAITING.store(true, std::sync::atomic::Ordering::SeqCst);
    // Simulated latency: SQLite calls the handler again at once, so back off between retries.
    std::thread::sleep(Duration::from_millis(1));
    true
}

#[test]
fn a_write_waits_for_another_connection_holding_the_file_instead_of_failing() {
    use std::sync::atomic::Ordering;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Store::open(&path, clock, Limits::default()).unwrap();
    // The write's own busy handler marks the moment it is blocked, and that releases the lock.
    lock(&store.conn)
        .busy_handler(Some(note_writer_is_waiting))
        .unwrap();
    // The other writer: the CLI, or the host a restart has not finished with.
    let holder = Connection::open(&path).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let release = std::thread::spawn(move || {
        puddle_test_kit::eventually_blocking(
            "the write to be blocked",
            Duration::from_secs(30),
            || WRITER_IS_WAITING.load(Ordering::SeqCst),
        );
        holder.execute_batch("COMMIT").unwrap();
    });
    let made = store.create_rule_set(UserId::OWNER, "Waits", "", None, Actor::Ui);
    release.join().unwrap();
    assert!(made.is_ok(), "{:?}", made.err());
    assert!(
        WRITER_IS_WAITING.load(Ordering::SeqCst),
        "the write ran only after the holder let go"
    );
}

#[test]
fn a_rule_set_made_by_an_unknown_actor_is_a_corrupt_row_not_text() {
    let (_, store) = store();
    store
        .create_rule_set(UserId::OWNER, "Odd", "", None, Actor::Ui)
        .unwrap();
    // The table refuses such a value; a hand-edited file may still hold one.
    let conn = lock(&store.conn);
    conn.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         UPDATE rule_sets SET created_by = 'robot';
         PRAGMA ignore_check_constraints = OFF;",
    )
    .unwrap();
    drop(conn);
    let err = store
        .update_rule_set(UserId::OWNER, 1, "Odd", "", None, Actor::Ui)
        .unwrap_err();
    assert!(
        matches!(&err, StoreError::Corrupt { table: "rule_sets", reason, .. } if reason.contains("robot")),
        "{err}"
    );
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
                "INSERT INTO rules (scope, workspace_id, pattern_kind, pattern, effect, created_at, created_by, user_id)
                 VALUES ('global', NULL, 'exact', 'NOT A HOST', 'allow', 0, 'cli', 1)",
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
    assert_eq!(
        store.rules_every_user(&crate::testing::all_users()),
        Vec::<Rule>::new()
    );
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
        enabled: 1,
        precedence: 5,
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
        program: None,
        exact_only: false,
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
        |r| r.program = Some(" / ".into()),
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

/// The `connection` records that are not summaries.
fn plain_connections(store: &Store) -> usize {
    store
        .audit_lines(0, 100_000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| serde_json::from_str::<serde_json::Value>(&line).unwrap())
        .filter(|v| v["type"] == "connection" && v["reason"] != "suppressed")
        .count()
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

#[test]
fn allowed_today_counts_rule_allows_before_the_record_limit_and_resets_at_midnight() {
    let (clock, store) = fresh_store();
    store
        .set_workspace_owner(&WorkspaceId::new("a").unwrap(), UserId::OWNER)
        .unwrap();
    let sb = |reason| {
        ConnectionEvent::new(
            &request("a", "x.example"),
            ConnectionDecision::Allow,
            reason,
        )
    };
    // 205 allowed by rule: the record limit drops 5 records, the count keeps them.
    for _ in 0..205 {
        store
            .record_connection(&sb(ConnectionReason::Rule))
            .unwrap();
    }
    store
        .record_connection(&sb(ConnectionReason::PuddleRequest))
        .unwrap();
    let denied = ConnectionEvent::new(
        &request("a", "x.example"),
        ConnectionDecision::Deny,
        ConnectionReason::Rule,
    );
    store.record_connection(&denied).unwrap();
    store.record_connection(&puddle_event()).unwrap();
    // 1_000_000 ms is in day 0.
    assert_eq!(store.allowed_today(UserId::OWNER), (205, 0));
    clock.advance(86_400_000);
    assert_eq!(store.allowed_today(UserId::OWNER), (0, 86_400_000));
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
fn a_wall_clock_step_back_keeps_the_connection_limit_and_the_next_second_opens_a_new_window() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let limits = Limits {
        connection_records_per_second: 1,
        ..Limits::default()
    };
    let store = Store::open_in_memory(clock.clone(), limits).unwrap();
    store.record_connection(&puddle_event()).unwrap();
    store.record_connection(&puddle_event()).unwrap();
    // Ten minutes back: still the same second of real time, so the second record stays refused.
    clock.set(400_000);
    store.record_connection(&puddle_event()).unwrap();
    assert_eq!(plain_connections(&store), 1);
    // A second later the window has moved on, though the wall clock still reads ten minutes back.
    clock.advance(1_000);
    store.record_connection(&puddle_event()).unwrap();
    assert_eq!(plain_connections(&store), 2);
    assert_eq!(summarised_connections(&store), 2);
    // A sweep reads the monotonic clock too: the second after that ends the window with excess.
    store.record_connection(&puddle_event()).unwrap();
    clock.advance(1_000);
    store.sweep().unwrap();
    assert_eq!(summarised_connections(&store), 3);
}

#[test]
fn the_audit_size_counts_the_search_text_a_long_path_adds() {
    let (_, store) = store();
    let before = store.audit_bytes().unwrap();
    let mut record = crate::audit::ConnectionRecord::from_event(
        1,
        &ConnectionEvent::new(
            &request("a", "x.example"),
            ConnectionDecision::Allow,
            ConnectionReason::Rule,
        ),
    );
    record.path = Some(format!("/{}", "p".repeat(5_000)));
    record.category = Some("company_network".into());
    let record = AuditRecord::connection(record);
    let line = record.to_line().unwrap().len() as u64;
    let search = record.search_text().unwrap().len() as u64;
    assert!(search > crate::MAX_FIELD_BYTES as u64 && search < 2 * crate::MAX_FIELD_BYTES as u64);
    append(&lock(&store.conn), &record).unwrap();
    assert_eq!(
        store.audit_bytes().unwrap() - before,
        line + search + "company_network".len() as u64
    );
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
        .as_user(ana.id)
        .add_rule(&NewRule {
            scope: Scope::Global,
            pattern: Pattern::Exact(host.clone()),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Ui,
        })
        .unwrap();

    match store.as_user(ana.id).peek(&workspace, &host) {
        Some(Decision::Allow { rule_id, .. }) => assert_eq!(rule_id, rule.id),
        other => panic!("expected the user's allow rule, got {other:?}"),
    }
    assert_eq!(store.as_user(ana.id).peek(&workspace, &elsewhere), None);
    assert_eq!(store.as_user(UserId::OWNER).peek(&workspace, &host), None);
}

#[test]
fn a_search_in_flight_on_the_audit_reader_does_not_hold_up_a_rule_change_and_its_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        Store::open(
            &dir.path().join("puddle.db"),
            Arc::new(ManualClock::new(1_000_000)),
            Limits::default(),
        )
        .unwrap(),
    );
    let op = NewWorkspaceOp {
        op: WorkspaceOpKind::Create,
        workspace: Some(WorkspaceId::new("acme").unwrap()),
        user: None,
        outcome: WorkspaceOpOutcome::Succeeded,
        actor: Actor::Ui,
        detail: None,
    };
    let allow = |store: &Store, host: &str| {
        store
            .as_user(UserId::OWNER)
            .add_rule(&NewRule {
                scope: Scope::Global,
                pattern: Pattern::Exact(Host::parse_normalised(host).unwrap()),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::Ui,
            })
            .map(|_| ())
    };
    allow(&store, "seed.example.com").unwrap();
    let search = |text: &str| {
        let filter = AuditFilter {
            q: Some(text.to_owned()),
            ..AuditFilter::default()
        };
        store
            .audit_query_every_user(
                &crate::testing::all_users(),
                &filter,
                AuditCursor::Before(None),
                100,
            )
            .unwrap()
    };
    assert_eq!(search("seed.example.com").len(), 1);

    // A search is a read transaction that stays open while it scans: hold one open here.
    let reader = store.audit_read();
    reader.execute_batch("BEGIN").unwrap();
    let before: i64 = reader
        .query_row("SELECT count(*) FROM audit", [], |row| row.get(0))
        .unwrap();

    let (done, finished) = std::sync::mpsc::channel();
    let writer = Arc::clone(&store);
    std::thread::spawn(move || {
        let rule = allow(&writer, "api.example.com");
        let _ = done.send((rule, writer.record_workspace_op(&op)));
    });
    // Generous: a writer that waited for the reader would never finish while it is held.
    let (rule, op_result) = finished
        .recv_timeout(Duration::from_secs(30))
        .expect("the writes finished while a read transaction was open");
    rule.unwrap();
    op_result.unwrap();

    // The open read transaction still sees its snapshot; a new one sees the rule's record.
    let during: i64 = reader
        .query_row("SELECT count(*) FROM audit", [], |row| row.get(0))
        .unwrap();
    assert_eq!(during, before);
    reader.execute_batch("COMMIT").unwrap();
    drop(reader);
    assert_eq!(search("api.example.com").len(), 1, "the rule's record");
    assert_eq!(
        search("seed.example.com").len(),
        1,
        "the first rule's record"
    );
}

#[test]
fn closing_a_store_that_read_the_audit_leaves_no_write_ahead_log() {
    // The read-only audit connection must close before the writer: it cannot delete the log.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = Store::open(
        &path,
        Arc::new(ManualClock::new(1_000_000)),
        Limits::default(),
    )
    .unwrap();
    store
        .record_workspace_op(&NewWorkspaceOp {
            op: WorkspaceOpKind::Rename,
            workspace: None,
            user: None,
            outcome: WorkspaceOpOutcome::Succeeded,
            actor: Actor::Ui,
            detail: None,
        })
        .unwrap();
    store.audit_workspace_names(UserId::OWNER, &[1]).unwrap(); // reads through the read-only connection
    drop(store);
    assert!(!dir.path().join("puddle.db-wal").exists());
}

fn partial_copy(dir: &Path, name: &str, age: Duration) -> std::path::PathBuf {
    let path = dir.join(name);
    let file = std::fs::File::create(&path).unwrap();
    file.set_modified(std::time::SystemTime::now() - age)
        .unwrap();
    path
}

#[test]
fn an_open_deletes_the_half_made_upgrade_copy_a_crash_left_but_not_one_still_being_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let crashed = partial_copy(
        dir.path(),
        "store.db.pre-v9.4242-0.partial",
        Duration::from_hours(3),
    );
    let running = partial_copy(
        dir.path(),
        "store.db.pre-v9.4243-0.partial",
        Duration::from_secs(5),
    );
    let other = partial_copy(
        dir.path(),
        "other.db.pre-v9.1-0.partial",
        Duration::from_secs(9999),
    );
    let clock = Arc::new(ManualClock::new(1_000_000));
    Store::open(&path, clock, Limits::default()).unwrap();
    assert!(!crashed.exists(), "the crash leftover is deleted");
    assert!(running.exists(), "a copy another process is writing stays");
    assert!(other.exists(), "another database's file stays");
}

/// A clock that, on its first reading after `arm`, runs a write through another handle on the
/// same file: another process writing between a setter's start and its transaction.
struct RaceClock {
    clock: ManualClock,
    hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Clock for RaceClock {
    fn now_ms(&self) -> u64 {
        let hook = lock(&self.hook).take();
        if let Some(hook) = hook {
            hook();
        }
        self.clock.now_ms()
    }
}

#[derive(Default)]
struct Emitted(Mutex<Vec<Event>>);

impl EventSink for Emitted {
    fn emit(&self, event: Event) {
        lock(&self.0).push(event);
    }
}

/// Two handles on one file. `rival` does the same write as `ours` just before ours starts, so
/// ours finds the value already there and has nothing to report.
fn race(
    rival: impl FnOnce(&Store) + Send + 'static,
    ours: impl FnOnce(&Store),
) -> (Store, Vec<Event>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.db");
    let clock = Arc::new(RaceClock {
        clock: ManualClock::new(1_000_000),
        hook: Mutex::new(None),
    });
    let emitted = Arc::new(Emitted::default());
    let store = Store::open(&path, clock.clone(), Limits::default())
        .unwrap()
        .with_events(emitted.clone());
    let other = Store::open(
        &path,
        Arc::new(ManualClock::new(1_000_000)),
        Limits::default(),
    )
    .unwrap();
    store
        .set_workspace_name(&WorkspaceId::new("a").unwrap(), "A")
        .unwrap();
    lock(&emitted.0).clear();
    *lock(&clock.hook) = Some(Box::new(move || rival(&other)));
    ours(&store);
    let events = std::mem::take(&mut *lock(&emitted.0));
    drop(dir);
    (store, events)
}

fn ws_a() -> WorkspaceId {
    WorkspaceId::new("a").unwrap()
}

#[test]
fn a_workspace_mode_already_saved_by_another_writer_sends_no_event() {
    let mode = Some(puddle_types::InterceptionMode::All);
    let (_, events) = race(
        move |other| other.set_workspace_mode(&ws_a(), mode).unwrap(),
        |store| store.set_workspace_mode(&ws_a(), mode).unwrap(),
    );
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn never_decrypt_names_already_saved_by_another_writer_send_no_event() {
    let names = vec!["bank.example".to_owned()];
    let other_names = names.clone();
    let (_, events) = race(
        move |other| other.set_never_decrypt(&ws_a(), &other_names).unwrap(),
        |store| store.set_never_decrypt(&ws_a(), &names).unwrap(),
    );
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn learned_limits_already_saved_by_another_writer_send_no_event() {
    let limits = LearnedLimits {
        ttl_secs: 99,
        cap: 7,
    };
    let (_, events) = race(
        move |other| other.set_learned_limits(limits).unwrap(),
        |store| store.set_learned_limits(limits).unwrap(),
    );
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn interception_settings_already_saved_by_another_writer_send_no_event() {
    let settings = InterceptionSettings {
        mode: Some(puddle_types::InterceptionMode::All),
        learn_from_rejections: false,
        notice_seen: true,
    };
    let (_, events) = race(
        move |other| other.set_interception_settings(settings).unwrap(),
        |store| store.set_interception_settings(settings).unwrap(),
    );
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn a_shell_init_already_saved_by_another_writer_sends_no_event() {
    let text = crate::ShellInitText::new("export A=1").unwrap();
    let other_text = text.clone();
    let (_, events) = race(
        move |other| {
            other.set_shell_init(&ws_a(), &other_text).unwrap();
        },
        |store| {
            store.set_shell_init(&ws_a(), &text).unwrap();
        },
    );
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn a_system_managed_plan_already_saved_by_another_writer_is_not_recorded_twice() {
    let plan = SystemPlan {
        everywhere: [crate::SystemReason::CodeServer].into(),
        ..SystemPlan::default()
    };
    let other_plan = plan.clone();
    let (store, _) = race(
        move |other| {
            other.set_system_managed(&other_plan).unwrap();
        },
        |store| {
            assert_eq!(store.set_system_managed(&plan).unwrap(), vec![]);
        },
    );
    let changes = store
        .audit_lines(0, 100)
        .unwrap()
        .iter()
        .filter(|(_, line)| line.contains("system_managed_changed"))
        .count();
    assert_eq!(changes, 1, "one record for one change");
}

#[test]
fn a_rule_change_shares_the_built_in_entries_instead_of_rebuilding_them() {
    let (_, store) = store();
    let before = store.snapshot();
    let w = WorkspaceId::new("a").unwrap();
    crate::testing::own(&store, &w);
    store
        .as_user(UserId::OWNER)
        .add_rule(&NewRule {
            scope: Scope::Workspace(w),
            pattern: Pattern::parse("x.example").unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
    let after = store.snapshot();
    assert!(!Arc::ptr_eq(&before, &after), "the snapshot is replaced");
    assert!(Arc::ptr_eq(before.built_in(), after.built_in()));
}

#[test]
fn a_page_of_built_in_entries_is_searched_offset_and_counted() {
    let (_, store) = store();
    let github = puddle_types::RuleSetId::BuiltIn("github");
    let all = store
        .built_in_entries(UserId::OWNER, github, "", 0, 1000)
        .unwrap();
    assert!(all.total >= 2 && all.entries.len() == all.total);
    let info = store.rule_set(UserId::OWNER, github).unwrap();
    assert_eq!(info.entry_count, all.total);
    let first = all.entries[0].pattern.clone();
    let one = store
        .built_in_entries(UserId::OWNER, github, &first.to_ascii_uppercase(), 0, 1)
        .unwrap();
    assert_eq!(one.entries.len(), 1);
    assert_eq!(one.entries[0].pattern, first);
    assert!(one.total >= 1 && one.total <= all.total);
    let rest = store
        .built_in_entries(UserId::OWNER, github, "", 1, 1000)
        .unwrap();
    assert_eq!(rest.entries, all.entries[1..]);
    assert_eq!(rest.total, all.total);
    let none = store
        .built_in_entries(UserId::OWNER, github, "no-such-host.invalid", 0, 10)
        .unwrap();
    assert_eq!((none.total, none.entries.len()), (0, 0));
}

#[test]
fn built_in_entries_refuse_a_set_that_is_not_built_in() {
    let (_, store) = store();
    assert!(matches!(
        store.built_in_entries(UserId::OWNER, puddle_types::RuleSetId::User(1), "", 0, 10),
        Err(StoreError::UnknownRuleSet(_))
    ));
    assert!(matches!(
        store.built_in_entries(UserId::OWNER, puddle_types::RuleSetId::System, "", 0, 10),
        Err(StoreError::UnknownRuleSet(_))
    ));
}

#[test]
fn the_entries_of_a_hidden_built_in_set_are_not_served() {
    let (_, store) = store();
    let github = puddle_types::RuleSetId::BuiltIn("github");
    assert!(
        store
            .built_in_entries(UserId::OWNER, github, "", 0, 10)
            .is_ok()
    );
    store
        .set_system_managed(&SystemPlan {
            hidden_sets: ["github"].into(),
            ..SystemPlan::default()
        })
        .unwrap();
    assert!(matches!(
        store.built_in_entries(UserId::OWNER, github, "", 0, 10),
        Err(StoreError::UnknownRuleSet(_))
    ));
}

#[test]
fn the_rule_set_list_never_mixes_a_switch_from_before_a_commit_with_rows_from_after() {
    let (_, store) = store();
    let before = store.rule_sets(UserId::OWNER).unwrap();
    let target = before[0].id;
    assert!(before[0].global.is_none(), "no switch yet");
    let seen = std::thread::scope(|scope| {
        // A commit in flight: it holds the lock and swaps the index before it lets go.
        let conn = lock(&store.conn);
        let reader = scope.spawn(|| store.rule_sets(UserId::OWNER).unwrap());
        // Ordering with no signal: the reader has no event to say it reached the lock, so give it time to block on it.
        std::thread::sleep(Duration::from_millis(300));
        conn.execute(
            "INSERT INTO rule_set_switches (rule_set, workspace_id, enabled, changed_at, user_id)
             VALUES (?1, NULL, 1, 0, 1)",
            [target.to_string()],
        )
        .unwrap();
        let index = sets::load_index(&conn).unwrap();
        *store.rules.write().unwrap() = Arc::new(index);
        drop(conn);
        reader.join().unwrap()
    });
    let listed = seen.iter().find(|s| s.id == target).unwrap();
    assert_eq!(listed.global, Some(true), "the list reflects the commit");
}

#[test]
fn one_rule_set_is_read_with_the_index_of_the_same_lock() {
    let (_, store) = store();
    let made = store
        .create_rule_set(UserId::OWNER, "Mine", "", None, Actor::Ui)
        .unwrap();
    assert!(made.global.is_none());
    let seen = std::thread::scope(|scope| {
        let conn = lock(&store.conn);
        let reader = scope.spawn(|| store.rule_set(UserId::OWNER, made.id).unwrap());
        // Ordering with no signal: the reader has no event to say it reached the lock, so give it time to block on it.
        std::thread::sleep(Duration::from_millis(300));
        conn.execute(
            "INSERT INTO rule_set_switches (rule_set, workspace_id, enabled, changed_at, user_id)
             VALUES (?1, NULL, 0, 0, 1)",
            [made.id.to_string()],
        )
        .unwrap();
        let index = sets::load_index(&conn).unwrap();
        *store.rules.write().unwrap() = Arc::new(index);
        drop(conn);
        reader.join().unwrap()
    });
    assert_eq!(seen.global, Some(false));
}
