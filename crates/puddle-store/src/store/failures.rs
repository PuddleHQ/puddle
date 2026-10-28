// SPDX-License-Identifier: GPL-3.0-or-later
//! What the store does when SQLite fails in the middle of a change: the error comes back as it is
//! and the change leaves nothing behind.

use puddle_types::{ConnectionDecision, ConnectionReason, PatternKind, RuleSetId};

use super::*;
use crate::clock::ManualClock;
use crate::test_support::Logs;

const DISK_FULL: &str = "database error: disk full";

fn store_with(limits: Limits) -> (Arc<ManualClock>, Store) {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let store = Store::open_in_memory(clock.clone(), limits).unwrap();
    (clock, store)
}

fn store() -> Store {
    store_with(Limits::default()).1
}

fn workspace() -> WorkspaceName {
    WorkspaceName::new("box").unwrap()
}

fn request(host: &str) -> EgressRequest {
    EgressRequest::new(workspace(), Host::parse_normalised(host).unwrap(), 443)
}

fn new_pending(store: &Store, host: &str) -> PendingId {
    let decision = store.decide(&request(host), SuffixAllows::Count).unwrap();
    assert!(matches!(
        decision,
        Decision::Pending(PendingOutcome::New(_))
    ));
    let open = store.open_pending(Some(&workspace())).unwrap();
    open.into_iter()
        .find(|row| row.host.to_string() == host)
        .unwrap()
        .id
}

fn workspace_rule(host: &str) -> NewRule {
    NewRule {
        scope: Scope::Workspace(workspace()),
        pattern: Pattern::parse(host).unwrap(),
        effect: Effect::Allow,
        expires_at: None,
        created_by: Actor::Cli,
    }
}

/// The `type` of every audit record, oldest first.
fn audit_kinds(store: &Store) -> Vec<String> {
    store
        .audit_lines(0, 1000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| {
            let record: serde_json::Value = serde_json::from_str(&line).unwrap();
            record["type"].as_str().unwrap().to_owned()
        })
        .collect()
}

const PENDING_UPDATES_FAIL: &str = "CREATE TRIGGER pending_updates_fail BEFORE UPDATE ON pending
     BEGIN SELECT RAISE(ABORT, 'disk full'); END";

#[test]
fn the_open_row_cap_is_the_one_the_store_was_opened_with() {
    let (_, store) = store_with(Limits {
        max_open_rows: 7,
        ..Limits::default()
    });
    assert_eq!(store.max_open_rows(), 7);
}

#[test]
fn a_rule_that_arrived_while_a_request_waited_for_the_database_decides_it() {
    let (_, waiting) = store_with(Limits::default());
    let (_, committed) = store_with(Limits::default());
    let rule = committed.add_rule(&workspace_rule("x.example")).unwrap();
    // The snapshot the waiting request finds once it has the lock.
    *waiting.rules.write().unwrap() = committed.snapshot();

    let decision = waiting
        .decide_locked(
            &mut lock(&waiting.conn),
            &request("x.example"),
            1_000_000,
            SuffixAllows::Count,
        )
        .unwrap();

    assert_eq!(
        decision,
        Decision::Allow {
            rule_id: rule.id,
            pattern: PatternKind::Exact
        }
    );
    assert!(
        waiting.open_pending(None).unwrap().is_empty(),
        "no pending row was made"
    );
}

#[test]
fn a_repeat_request_that_cannot_be_counted_leaves_its_row_as_it_was() {
    let store = store();
    let id = new_pending(&store, "x.example");
    store.break_database(PENDING_UPDATES_FAIL);

    let err = store
        .decide(&request("x.example"), SuffixAllows::Count)
        .unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.pending(id).unwrap().attempts, 1);
}

#[test]
fn a_request_whose_open_rows_cannot_be_counted_makes_no_row() {
    let store = store();
    store.refuse_sql_function("count");

    let err = store
        .decide(&request("x.example"), SuffixAllows::Count)
        .unwrap_err();

    assert!(
        err.to_string()
            .starts_with("database error: not authorized to use function: count"),
        "{err}"
    );
    assert_eq!(store.open_pending(None).unwrap().len(), 0);
}

#[test]
fn a_new_row_whose_record_cannot_be_written_is_not_made() {
    let store = store();
    store.fail_audit_writes("pending_created");

    let err = store
        .decide(&request("x.example"), SuffixAllows::Count)
        .unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.open_pending(None).unwrap().len(), 0);
    assert_eq!(audit_kinds(&store), Vec::<String>::new());
}

#[test]
fn a_rule_that_cannot_be_inserted_is_not_made() {
    let store = store();
    store.break_database(
        "CREATE TRIGGER rule_inserts_fail BEFORE INSERT ON rules
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    );

    let err = store.add_rule(&workspace_rule("x.example")).unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), Vec::<Rule>::new());
}

#[test]
fn a_rule_whose_record_cannot_be_written_is_not_made() {
    let store = store();
    store.fail_audit_writes("rule_created");

    let err = store.add_rule(&workspace_rule("x.example")).unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), Vec::<Rule>::new());
}

#[test]
fn a_rule_whose_deletion_cannot_be_recorded_stays() {
    let store = store();
    let rule = store.add_rule(&workspace_rule("x.example")).unwrap();
    store.fail_audit_writes("rule_deleted");

    let err = store.delete_rule(rule.id, Actor::Cli).unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), [rule]);
}

#[test]
fn a_rule_whose_expiry_change_cannot_be_recorded_keeps_its_expiry() {
    let store = store();
    let rule = store.add_rule(&workspace_rule("x.example")).unwrap();
    store.fail_audit_writes("rule_updated");

    let err = store
        .set_rule_expiry(rule.id, Some(2_000_000), Actor::Cli)
        .unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), [rule]);
}

#[test]
fn a_decision_that_cannot_update_its_row_makes_no_rule() {
    let store = store();
    let id = new_pending(&store, "x.example");
    store.break_database(PENDING_UPDATES_FAIL);

    let err = store
        .resolve_pending(id, &Resolution::allow(), Actor::Ui)
        .unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), Vec::<Rule>::new());
    assert_eq!(store.pending(id).unwrap().state, PendingState::Requested);
}

#[test]
fn a_decision_whose_record_cannot_be_written_makes_no_rule() {
    let store = store();
    let id = new_pending(&store, "x.example");
    store.fail_audit_writes("pending_decided");

    let err = store
        .resolve_pending(id, &Resolution::allow(), Actor::Ui)
        .unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), Vec::<Rule>::new());
    assert_eq!(store.pending(id).unwrap().state, PendingState::Requested);
}

#[test]
fn a_decision_is_logged_with_its_effect() {
    let store = store();
    let id = new_pending(&store, "x.example");
    let logs = Logs::capture();

    let decided = store
        .resolve_pending(id, &Resolution::allow(), Actor::Ui)
        .unwrap();

    assert_eq!(
        logs.lines(),
        [format!(
            "INFO pending request decided pending={id} workspace=box host=x.example rule={} effect=\"allow\"",
            decided.rule.id
        )]
    );
}

#[test]
fn a_rule_in_a_set_that_is_off_leaves_the_open_requests_it_matches_open() {
    let store = store();
    let created = store.create_rule_set("Blocks", "", Actor::Ui).unwrap();
    let set: i64 = created.id.to_string()[5..].parse().unwrap();
    store
        .switch_rule_set(RuleSetId::User(set), None, Some(false), Actor::Ui)
        .unwrap();
    let id = new_pending(&store, "x.example");

    store
        .add_rule(&NewRule {
            scope: Scope::Set(set),
            ..workspace_rule("x.example")
        })
        .unwrap();

    assert_eq!(store.pending(id).unwrap().state, PendingState::Requested);
}

#[test]
fn a_workspace_whose_rule_deletion_cannot_be_recorded_keeps_everything() {
    let store = store();
    let rule = store.add_rule(&workspace_rule("x.example")).unwrap();
    store.fail_audit_writes("rule_deleted");

    let err = store.delete_workspace(&workspace()).unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), [rule]);
}

#[test]
fn a_workspace_whose_open_requests_cannot_be_expired_keeps_everything() {
    let store = store();
    let rule = store.add_rule(&workspace_rule("x.example")).unwrap();
    let id = new_pending(&store, "y.example");
    store.fail_audit_writes("pending_expired");

    let err = store.delete_workspace(&workspace()).unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), [rule]);
    assert_eq!(store.pending(id).unwrap().state, PendingState::Requested);
}

#[test]
fn a_workspace_whose_pinned_ports_cannot_be_removed_keeps_everything() {
    let store = store();
    let rule = store.add_rule(&workspace_rule("x.example")).unwrap();
    let pin = PinnedPort {
        guest_port: 3000,
        host_port: 3000,
    };
    store.pin_port(&workspace(), pin).unwrap();
    store.break_database(
        "CREATE TRIGGER pin_deletes_fail BEFORE DELETE ON pinned_ports
         BEGIN SELECT RAISE(ABORT, 'disk full'); END",
    );

    let err = store.delete_workspace(&workspace()).unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.rules(), [rule]);
    assert_eq!(store.pinned_ports(&workspace()).unwrap(), [pin]);
}

fn waiting_event(host: &str) -> ConnectionEvent {
    ConnectionEvent::new(
        &request(host),
        ConnectionDecision::Pending,
        ConnectionReason::NoRule,
    )
}

#[test]
fn waiting_records_stay_unmerged_when_their_summary_cannot_be_written() {
    let (_, store) = store_with(Limits {
        max_waiting_connection_records: 1,
        ..Limits::default()
    });
    for host in ["a.example", "b.example", "c.example"] {
        store.record_connection(&waiting_event(host)).unwrap();
    }
    store.fail_audit_writes("connection");

    let err = store.sweep().unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(audit_kinds(&store), ["connection"; 3]);
    store.audit_writes_work_again();
    assert_eq!(store.sweep().unwrap().waiting_records_merged, 2);
}

#[test]
fn an_audit_trim_whose_record_cannot_be_written_still_trimmed() {
    let (_, store) = store_with(Limits {
        audit_max_bytes: 1,
        audit_trim_to_bytes: 0,
        ..Limits::default()
    });
    store.add_rule(&workspace_rule("x.example")).unwrap();
    store.fail_audit_writes("audit_trimmed");

    let err = store.sweep().unwrap_err();

    assert_eq!(err.to_string(), DISK_FULL);
    assert_eq!(store.audit_bytes().unwrap(), 0);
}

#[test]
fn the_policy_reports_a_failed_decision_as_unavailable() {
    let store = store();
    store.fail_audit_writes("pending_created");

    let err = Policy::decide(&store, &request("x.example"), SuffixAllows::Count).unwrap_err();

    assert_eq!(
        err,
        PolicyError {
            reason: DISK_FULL.into()
        }
    );
}

#[test]
fn a_connection_record_that_cannot_be_written_is_logged_and_dropped() {
    let store = store();
    store.fail_audit_writes("connection");
    let logs = Logs::capture();

    ConnectionLog::record(&store, &waiting_event("x.example"));

    assert_eq!(
        logs.lines(),
        [format!(
            "WARN connection record not written origin={} error={DISK_FULL}",
            ConnectionOrigin::Workspace
        )]
    );
    assert_eq!(audit_kinds(&store), Vec::<String>::new());
}

#[test]
fn pinned_ports_that_cannot_be_read_are_a_database_error() {
    let store = store();
    store.break_database("DROP TABLE pinned_ports");

    let err = store.pinned_ports(&workspace()).unwrap_err();

    assert_eq!(
        err.to_string(),
        "database error: no such table: pinned_ports"
    );
}
