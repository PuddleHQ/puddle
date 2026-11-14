// SPDX-License-Identifier: AGPL-3.0-or-later
use serde_json::{Value, json};

use super::*;

fn pending(state: &str) -> Value {
    json!({
        "id": 7, "workspace_id": "ws-1", "host": "example.com", "port": 443,
        "first_seen": 10, "last_seen": 20, "attempts": 3, "state": state,
        "decided_at": null, "decided_by": null, "rule_id": null,
    })
}

fn rule(id: i64, pattern: &str) -> Value {
    json!({
        "id": id, "scope": "global", "workspace_id": null, "pattern_kind": "exact",
        "pattern": pattern, "effect": "allow", "expires_at": null, "created_at": 5,
        "created_by": "ui", "source_pending_id": null,
    })
}

/// What the API serves for a record the store holds.
fn served(stored: &Value) -> Value {
    let record: store::AuditRecord = serde_json::from_value(stored.clone()).unwrap();
    serde_json::to_value(AuditRecord::from(record)).unwrap()
}

#[test]
fn an_expired_request_keeps_its_row_and_the_reason_under_its_own_name() {
    for (stored_reason, served_reason) in [
        ("stale", "stale"),
        ("workspace_deleted", "workspace_deleted"),
    ] {
        let out = served(&json!({
            "type": "pending_expired", "ts": 99, "pending": pending("expired"),
            "reason": stored_reason,
        }));
        assert_eq!(out["type"], "pending_expired");
        assert_eq!(out["ts"], 99);
        assert_eq!(out["reason"], served_reason);
        assert_eq!(out["pending"]["id"], 7);
        assert_eq!(out["pending"]["host"], "example.com");
        assert_eq!(out["pending"]["state"], "expired");
    }
}

#[test]
fn a_record_of_suppressed_requests_still_reads_with_its_count() {
    let out = served(&json!({
        "type": "pending_suppressed", "ts": 5, "sandbox_id": "old-name", "count": 12,
    }));
    assert_eq!(
        out,
        json!({"type": "pending_suppressed", "ts": 5, "workspace_id": "old-name", "count": 12})
    );
}

#[test]
fn a_changed_rule_carries_both_versions_and_who_changed_it() {
    let out = served(&json!({
        "type": "rule_updated", "ts": 8, "before": rule(1, "a.example"),
        "rule": rule(1, "b.example"), "actor": "cli",
    }));
    assert_eq!(out["type"], "rule_updated");
    assert_eq!(out["before"]["pattern"], "a.example");
    assert_eq!(out["rule"]["pattern"], "b.example");
    assert_eq!(out["actor"], "cli");
    assert_eq!(out["ts"], 8);
}

#[test]
fn an_expired_rule_is_served_whole() {
    let out = served(&json!({"type": "rule_expired", "ts": 9, "rule": rule(4, "gone.example")}));
    assert_eq!(out["type"], "rule_expired");
    assert_eq!(out["ts"], 9);
    assert_eq!(out["rule"]["id"], 4);
    assert_eq!(out["rule"]["pattern"], "gone.example");
}

#[test]
fn a_trim_of_the_audit_says_how_many_went_and_where_it_starts_now() {
    for kept in [json!(1234), Value::Null] {
        let out = served(&json!({
            "type": "audit_trimmed", "ts": 3, "deleted_records": 500, "oldest_ts_kept": kept,
        }));
        assert_eq!(
            out,
            json!({
                "type": "audit_trimmed", "ts": 3, "deleted_records": 500,
                "oldest_ts_kept": kept,
            })
        );
    }
}

#[test]
fn a_new_request_and_a_new_rule_are_served_with_their_rows() {
    let created =
        served(&json!({"type": "pending_created", "ts": 1, "pending": pending("requested")}));
    assert_eq!(created["type"], "pending_created");
    assert_eq!(created["pending"]["id"], 7);
    assert_eq!(created["pending"]["port"], 443);
    assert_eq!(created["pending"]["state"], "requested");

    let made = served(&json!({"type": "rule_created", "ts": 2, "rule": rule(3, "new.example")}));
    assert_eq!(made["type"], "rule_created");
    assert_eq!(made["ts"], 2);
    assert_eq!(made["rule"]["id"], 3);
    assert_eq!(made["rule"]["pattern"], "new.example");
}
