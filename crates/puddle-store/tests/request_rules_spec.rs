// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules on the method and path of a request (`docs/spec/rules.md` R-49..R-55), through the
//! store's public API.
#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] functions fail the test by panicking"
)]

use std::sync::Arc;

use puddle_store::{
    Actor, AuditCursor, AuditFilter, Limits, MAX_REQUEST_RULES, ManualClock, NewRequestRule, Store,
    StoreError,
};
use puddle_types::{
    CollectingSink, ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionReason,
    EgressRequest, Event, Host, HttpRequestLine, Methods, PathPattern, RuleId, WorkspaceName,
};
use serde_json::{Value, json};

const T0: u64 = 1_800_000_000_000;

fn sb(id: &str) -> WorkspaceName {
    WorkspaceName::new(id).unwrap()
}

fn fixture() -> (Arc<CollectingSink>, Store) {
    let sink = Arc::new(CollectingSink::default());
    let store = Store::open_in_memory(Arc::new(ManualClock::new(T0)), Limits::default())
        .unwrap()
        .with_events(sink.clone());
    (sink, store)
}

fn new_rule(workspace: Option<&str>, host: &str, methods: &[&str], path: &str) -> NewRequestRule {
    NewRequestRule {
        workspace: workspace.map(sb),
        host: host.to_owned(),
        methods: Methods::parse(methods.iter().copied()).unwrap(),
        path: PathPattern::parse(path).unwrap(),
        created_by: Actor::Api,
    }
}

fn records(store: &Store) -> Vec<Value> {
    store
        .audit_query(&AuditFilter::default(), AuditCursor::After(0), 1000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| serde_json::from_str(&line).unwrap())
        .collect()
}

fn changed(sink: &CollectingSink) -> usize {
    sink.take()
        .iter()
        .filter(|event| matches!(event, Event::RequestRulesChanged {}))
        .count()
}

#[test]
fn a_rule_is_kept_with_its_own_numbering_and_listed_by_scope() {
    let (sink, store) = fixture();
    let global = store
        .add_request_rule(&new_rule(None, "api.example.com", &["delete"], "/repos/**"))
        .unwrap();
    let own = store
        .add_request_rule(&new_rule(Some("shop"), "*.example.org", &[], "/a/*"))
        .unwrap();
    store
        .add_request_rule(&new_rule(Some("other"), "x.example.net", &[], "/"))
        .unwrap();
    assert_eq!((global.id, own.id), (1, 2));
    assert_eq!(global.methods.names(), ["DELETE"]);
    assert_eq!(global.created_by, Actor::Api);
    assert_eq!(changed(&sink), 3, "each change tells running workspaces");

    let all = store.request_rules(None).unwrap();
    assert_eq!(all.len(), 3);
    let for_shop: Vec<i64> = store
        .request_rules(Some(&sb("shop")))
        .unwrap()
        .iter()
        .map(|rule| rule.id)
        .collect();
    assert_eq!(for_shop, [1, 2], "the global rules and its own, by id");
}

#[test]
fn a_rule_is_refused_from_the_system_with_no_host_or_over_the_limit() {
    let (sink, store) = fixture();
    let mut system = new_rule(None, "a.example.com", &[], "/");
    system.created_by = Actor::System;
    assert!(matches!(
        store.add_request_rule(&system),
        Err(StoreError::SystemActor)
    ));
    assert!(matches!(
        store.add_request_rule(&new_rule(None, "  ", &[], "/")),
        Err(StoreError::RequestRuleInvalid(_))
    ));
    for i in 0..MAX_REQUEST_RULES {
        store
            .add_request_rule(&new_rule(
                Some("shop"),
                &format!("h{i}.example.com"),
                &[],
                "/",
            ))
            .unwrap();
    }
    assert!(matches!(
        store.add_request_rule(&new_rule(Some("shop"), "more.example.com", &[], "/")),
        Err(StoreError::RequestRuleInvalid(_))
    ));
    store
        .add_request_rule(&new_rule(None, "global.example.com", &[], "/"))
        .unwrap();
    store
        .add_request_rule(&new_rule(Some("other"), "more.example.com", &[], "/"))
        .unwrap();
    assert_eq!(
        changed(&sink),
        MAX_REQUEST_RULES + 2,
        "a refused rule changes nothing and tells no one"
    );
}

#[test]
fn deleting_a_rule_is_recorded_with_who_did_it() {
    let (sink, store) = fixture();
    let rule = store
        .add_request_rule(&new_rule(None, "api.example.com", &["PUT"], "/x"))
        .unwrap();
    drop(sink.take());
    assert!(matches!(
        store.delete_request_rule(rule.id, Actor::System),
        Err(StoreError::SystemActor)
    ));
    assert!(matches!(
        store.delete_request_rule(99, Actor::Cli),
        Err(StoreError::UnknownRequestRule(99))
    ));
    assert_eq!(changed(&sink), 0);
    let gone = store.delete_request_rule(rule.id, Actor::Cli).unwrap();
    assert_eq!(gone.id, rule.id);
    assert_eq!(changed(&sink), 1);
    assert_eq!(store.request_rules(None).unwrap().len(), 0);
    let log = records(&store);
    assert_eq!(log[0]["type"], "request_rule_created");
    assert_eq!(log[1]["type"], "request_rule_deleted");
    assert_eq!(log[1]["rule"]["id"], 1);
    assert_eq!(log[1]["rule"]["methods"], json!(["PUT"]));
    assert_eq!(log[1]["reason"], "user");
    assert_eq!(log[1]["actor"], "cli");
}

#[test]
fn deleting_a_workspace_takes_its_rules_and_keeps_the_global_ones() {
    let (sink, store) = fixture();
    store
        .add_request_rule(&new_rule(None, "g.example.com", &[], "/"))
        .unwrap();
    store
        .add_request_rule(&new_rule(Some("shop"), "a.example.com", &[], "/a"))
        .unwrap();
    store
        .add_request_rule(&new_rule(Some("shop"), "b.example.com", &[], "/b"))
        .unwrap();
    drop(sink.take());
    store.delete_workspace(&sb("shop")).unwrap();
    assert_eq!(changed(&sink), 1, "one notice for the workspace's rules");
    let left = store.request_rules(None).unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].host, "g.example.com");
    let log = records(&store);
    let deletions: Vec<&Value> = log
        .iter()
        .filter(|r| r["type"] == "request_rule_deleted")
        .collect();
    assert_eq!(deletions.len(), 2, "each rule has its own record");
    for record in deletions {
        assert_eq!(record["reason"], "workspace_deleted");
        assert_eq!(record["actor"], "system");
    }
    // A workspace with no rules sends no notice.
    drop(sink.take());
    store.delete_workspace(&sb("empty")).unwrap();
    assert_eq!(changed(&sink), 0);
}

#[test]
fn a_refused_request_is_the_connections_record_with_its_rule_method_and_path() {
    let (_, store) = fixture();
    let request = EgressRequest::new(
        sb("shop"),
        Host::parse_normalised("api.example.com").unwrap(),
        443,
    );
    let mut event = ConnectionEvent::new(
        &request,
        ConnectionDecision::Deny,
        ConnectionReason::RequestRule,
    );
    event.request_rule = Some(RuleId(7));
    event.http = Some(HttpRequestLine::new("DELETE", "/repos/a/b"));
    store.record(&event);
    let log = records(&store);
    assert_eq!(log.len(), 1);
    let record = &log[0];
    assert_eq!(record["type"], "connection");
    assert_eq!(record["reason"], "request_rule");
    assert_eq!(record["request_rule"], 7);
    assert_eq!(
        record["rule_id"],
        Value::Null,
        "the host rules' numbering is not used"
    );
    assert_eq!(record["method"], "DELETE");
    assert_eq!(record["path"], "/repos/a/b");
    assert_eq!(record["decision"], "deny");
}

#[test]
fn a_row_that_cannot_be_read_back_is_reported_as_corrupt_and_not_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    let store = Store::open(&path, Arc::new(ManualClock::new(T0)), Limits::default()).unwrap();
    store
        .add_request_rule(&new_rule(None, "a.example.com", &["GET"], "/x"))
        .unwrap();
    drop(store);

    let conn = rusqlite::Connection::open(&path).unwrap();
    for (damage, said) in [
        ("path = 'no-slash'", "path"),
        ("methods = 'GE T'", "method"),
        ("created_at = -1", "negative time"),
    ] {
        conn.execute(&format!("UPDATE request_rules SET {damage}"), [])
            .unwrap();
        let store = Store::open(&path, Arc::new(ManualClock::new(T0)), Limits::default()).unwrap();
        let err = store.request_rules(None).unwrap_err();
        assert!(
            matches!(&err, StoreError::Corrupt { table: "request_rules", id: 1, reason } if reason.contains(said)),
            "{damage}: {err:?}"
        );
        drop(store);
        conn.execute(
            "UPDATE request_rules SET path = '/x', methods = 'GET', created_at = 1",
            [],
        )
        .unwrap();
    }
}

#[test]
fn a_column_of_the_wrong_type_is_an_error_for_the_listing_and_never_a_guess() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("puddle.db");
    drop(Store::open(&path, Arc::new(ManualClock::new(T0)), Limits::default()).unwrap());

    // One row per column, each with that column holding a value of the wrong type.
    let columns = [
        "id",
        "workspace_id",
        "host",
        "methods",
        "path",
        "created_at",
        "created_by",
    ];
    let good = [
        "1",
        "NULL",
        "'a.example.com'",
        "'GET'",
        "'/x'",
        "1",
        "'api'",
    ];
    let wrong = ["'one'", "5", "NULL", "NULL", "NULL", "NULL", "NULL"];
    let conn = rusqlite::Connection::open(&path).unwrap();
    for (at, column) in columns.iter().enumerate() {
        conn.execute_batch(
            "DROP TABLE request_rules; \
             CREATE TABLE request_rules (id, scope, workspace_id, host, methods, path, created_at, created_by);",
        )
        .unwrap();
        let mut values = good.to_vec();
        values[at] = wrong[at];
        conn.execute_batch(&format!(
            "INSERT INTO request_rules VALUES ({}, 'global', {}, {}, {}, {}, {}, {})",
            values[0], values[1], values[2], values[3], values[4], values[5], values[6]
        ))
        .unwrap();
        let store = Store::open(&path, Arc::new(ManualClock::new(T0)), Limits::default()).unwrap();
        assert!(store.request_rules(None).is_err(), "{column}");
    }
}
