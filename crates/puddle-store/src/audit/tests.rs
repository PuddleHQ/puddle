// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use crate::pending::PendingState;
use proptest::prelude::*;
use puddle_types::{
    BlockReason, CompanySignal, EgressRequest, Host, HttpRequestLine, LocalCategory, RefusedReason,
    RuleId,
};
use serde_json::json;

const CANARY: &str = "CANARY-7f3a9c";

fn sb() -> WorkspaceId {
    WorkspaceId::new("sb-1").unwrap()
}

fn event() -> ConnectionEvent {
    let request = EgressRequest::new(
        sb(),
        Host::parse_normalised("api.example.com").unwrap(),
        443,
    );
    let mut event =
        ConnectionEvent::new(&request, ConnectionDecision::Allow, ConnectionReason::Rule);
    event.resolved_ip = Some("93.184.216.34".parse().unwrap());
    event.rule_id = Some(RuleId(4));
    event.binding_id = Some("github".into());
    event.injected = true;
    event.http = Some(HttpRequestLine::new("GET", "/v1/items?id=1"));
    event.bytes_up = 10;
    event.bytes_down = 20;
    event
}

fn rule_wire() -> RuleWire {
    RuleWire {
        id: 1,
        scope: "workspace".into(),
        workspace_id: Some("sb-1".into()),
        pattern_kind: "exact".into(),
        pattern: "example.com".into(),
        effect: "allow".into(),
        expires_at: None,
        created_at: 5,
        created_by: "cli".into(),
        source_pending_id: Some(2),
        set_id: None,
    }
}

fn pending_wire() -> PendingWire {
    PendingWire {
        id: 2,
        workspace_id: "sb-1".into(),
        host: "example.com".into(),
        port: 443,
        first_seen: 1,
        last_seen: 2,
        attempts: 3,
        state: PendingState::Requested.as_str().into(),
        decided_at: None,
        decided_by: None,
        rule_id: None,
        rule_set: None,
    }
}

fn every_record() -> Vec<AuditRecord> {
    vec![
        AuditRecord::Connection(ConnectionRecord::from_event(9, &event())),
        AuditRecord::Connection(ConnectionRecord::suppressed_summary(9, Some(&sb()), 12)),
        AuditRecord::PendingCreated {
            ts: 9,
            pending: pending_wire(),
        },
        AuditRecord::PendingDecided {
            ts: 9,
            pending: pending_wire(),
        },
        AuditRecord::PendingExpired {
            ts: 9,
            pending: pending_wire(),
            reason: PendingExpiryReason::Stale,
        },
        AuditRecord::PendingSuppressed {
            ts: 9,
            workspace_id: "sb-1".into(),
            count: 3,
        },
        AuditRecord::RuleCreated {
            ts: 9,
            rule: rule_wire(),
        },
        AuditRecord::RuleUpdated {
            ts: 9,
            before: rule_wire(),
            rule: rule_wire(),
            actor: "ui".into(),
        },
        AuditRecord::RuleDeleted {
            ts: 9,
            rule: rule_wire(),
            reason: RuleDeleteReason::WorkspaceDeleted,
            actor: "system".into(),
        },
        AuditRecord::RuleExpired {
            ts: 9,
            rule: rule_wire(),
        },
        AuditRecord::AuditTrimmed {
            ts: 9,
            deleted_records: 100,
            oldest_ts_kept: None,
        },
    ]
}

/// Lines written before a workspace was called a sandbox in the audit still read: the owner's
/// key, the connection origin, the rule scope and the deletion reason, each under the old name.
#[test]
fn lines_written_with_the_old_names_still_read() {
    let rule = |scope: &str| {
        json!({"id": 1, "scope": scope, "sandbox_id": "sb-1", "pattern_kind": "exact",
                "pattern": "example.com", "effect": "allow", "expires_at": null, "created_at": 5,
                "created_by": "cli", "source_pending_id": null})
    };
    let pending = json!({"id": 2, "sandbox_id": "sb-1", "host": "example.com", "port": 443,
            "first_seen": 1, "last_seen": 2, "attempts": 3, "state": "requested",
            "decided_at": null, "decided_by": null, "rule_id": null});
    let old = [
        json!({"type": "connection", "ts": 9, "sandbox_id": "sb-1", "origin": "sandbox",
                "host": "example.com", "port": 443, "resolved_ip": null, "decision": "allow",
                "reason": "rule", "rule_id": 1, "pending_id": null, "binding_id": null,
                "injected": false, "method": null, "path": null, "path_truncated": false,
                "bytes_up": 0, "bytes_down": 0, "count": null}),
        json!({"type": "pending_expired", "ts": 9, "pending": pending, "reason": "sandbox_deleted"}),
        json!({"type": "pending_suppressed", "ts": 9, "sandbox_id": "sb-1", "count": 3}),
        json!({"type": "rule_deleted", "ts": 9, "rule": rule("sandbox"),
                "reason": "sandbox_deleted", "actor": "system"}),
        json!({"type": "rule_set_switched", "ts": 9, "set_id": "builtin:x",
                "sandbox_id": null, "enabled": true, "actor": "ui"}),
    ];
    for line in old {
        let record: AuditRecord = serde_json::from_value(line.clone()).unwrap();
        assert_eq!(
            record.workspace_id().is_some(),
            line["type"] != "rule_set_switched"
        );
        let written = serde_json::to_string(&record).unwrap();
        assert!(!written.contains("sandbox"), "{written}");
    }
    let record: AuditRecord =
        serde_json::from_value(json!({"type": "rule_created", "ts": 9, "rule": rule("sandbox")}))
            .unwrap();
    assert!(matches!(record, AuditRecord::RuleCreated { rule, .. } if rule.scope == "workspace"));
}

/// A line written before the list existed reads as no stand-in left unswapped, and a list
/// the event carried is kept in order.
#[test]
fn a_connection_line_without_the_stand_in_list_reads_as_empty_and_a_list_is_kept() {
    let line = json!({"type": "connection", "ts": 9, "workspace_id": "sb-1",
            "origin": "workspace", "host": "example.com", "port": 443, "resolved_ip": null,
            "decision": "allow", "reason": "rule", "rule_id": 1, "pending_id": null,
            "binding_id": null, "injected": false, "method": null, "path": null,
            "path_truncated": false, "bytes_up": 0, "bytes_down": 0, "count": null});
    let record: AuditRecord = serde_json::from_value(line).unwrap();
    assert!(matches!(
        record,
        AuditRecord::Connection(ConnectionRecord { unswapped_stand_ins, .. })
            if unswapped_stand_ins.is_empty()
    ));
    assert!(matches!(
        AuditRecord::Connection(ConnectionRecord::from_event(1, &event())),
        AuditRecord::Connection(ConnectionRecord { scrubbed_stand_ins, .. })
            if scrubbed_stand_ins.is_empty()
    ));
    let flagged = ConnectionRecord::from_event(1, &{
        let mut event = event();
        event.unswapped_stand_ins = vec![
            "stand-in:secret:A".to_owned(),
            "stand-in:secret:B".to_owned(),
        ];
        event
    });
    assert_eq!(
        flagged.unswapped_stand_ins,
        ["stand-in:secret:A", "stand-in:secret:B"]
    );
    assert!(matches!(
        AuditRecord::Connection(ConnectionRecord::from_event(1, &event())),
        AuditRecord::Connection(ConnectionRecord { foreign_credentials, .. })
            if foreign_credentials.is_empty()
    ));
    let mut warned = event();
    warned.foreign_credentials = vec!["github_token".to_owned()];
    assert_eq!(
        ConnectionRecord::from_event(1, &warned).foreign_credentials,
        ["github_token"]
    );
    let mut told = event();
    told.answer_credentials = vec!["jwt_with_refresh_token".to_owned()];
    assert_eq!(
        ConnectionRecord::from_event(1, &told).answer_credentials,
        ["jwt_with_refresh_token"]
    );
    assert_eq!(
        ConnectionRecord::from_event(1, &event()).answer_credentials,
        Vec::<String>::new()
    );
}

/// A line written before the field existed reads as no refused address, and an event's
/// refused address and reason reach the record.
#[test]
fn a_connection_line_without_a_refused_address_reads_as_none_and_one_is_kept() {
    let line = json!({"type": "connection", "ts": 9, "workspace_id": "sb-1",
            "origin": "workspace", "host": "example.com", "port": 443, "resolved_ip": null,
            "decision": "allow", "reason": "rule", "rule_id": 1, "pending_id": null,
            "binding_id": null, "injected": false, "method": null, "path": null,
            "path_truncated": false, "bytes_up": 0, "bytes_down": 0, "count": null});
    let record: AuditRecord = serde_json::from_value(line).unwrap();
    assert!(matches!(
        record,
        AuditRecord::Connection(ConnectionRecord {
            refused_address: None,
            refused_reason: None,
            ..
        })
    ));
    let mut mixed = event();
    mixed.refused_address = Some((
        "10.0.0.8".parse().unwrap(),
        BlockReason::LocalAddress.into(),
    ));
    let kept = ConnectionRecord::from_event(1, &mixed);
    assert_eq!(
        (
            kept.refused_address.as_deref(),
            kept.refused_reason.as_deref()
        ),
        (Some("10.0.0.8"), Some("local_address"))
    );
    // An address an IP rule denied keeps its own code, apart from the guard's.
    let mut denied = event();
    denied.refused_address = Some(("10.0.0.9".parse().unwrap(), RefusedReason::IpRule));
    let kept = ConnectionRecord::from_event(1, &denied);
    assert_eq!(
        (
            kept.refused_address.as_deref(),
            kept.refused_reason.as_deref()
        ),
        (Some("10.0.0.9"), Some("ip_rule"))
    );
}

#[test]
fn the_outcome_of_reaching_the_destination_is_kept_and_old_records_read_as_none() {
    let mut event = event();
    assert_eq!(ConnectionRecord::from_event(1, &event).outcome, None);
    for outcome in [
        ConnectionOutcome::Connected,
        ConnectionOutcome::UpstreamRefused,
        ConnectionOutcome::ConnectFailed,
        ConnectionOutcome::Timeout,
    ] {
        event.outcome = Some(outcome);
        let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &event))
            .to_line()
            .unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["outcome"], outcome.as_str(), "{line}");
        let back: AuditRecord = serde_json::from_str(&line).unwrap();
        assert!(
            matches!(&back, AuditRecord::Connection(r) if r.outcome == Some(outcome)),
            "{back:?}"
        );
    }
    // A record written before the field existed.
    let old = serde_json::json!({"type": "connection", "ts": 1, "workspace_id": "box",
            "host": "example.com", "port": 443, "resolved_ip": null, "decision": "allow",
            "reason": "rule", "rule_id": null, "pending_id": null, "binding_id": null,
            "injected": false, "method": null, "path": null, "path_truncated": false,
            "bytes_up": 0, "bytes_down": 0, "count": null});
    let old: AuditRecord = serde_json::from_value(old).unwrap();
    assert!(
        matches!(&old, AuditRecord::Connection(r) if r.outcome.is_none()),
        "{old:?}"
    );
}

#[test]
fn r23_records_are_internally_tagged_snake_case_with_nulls_written() {
    for record in every_record() {
        let line = record.to_line().unwrap();
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["type"], record.kind(), "{line}");
        assert_eq!(value["ts"], 9);
        let back: AuditRecord = serde_json::from_str(&line).unwrap();
        assert_eq!(back, record);
    }
    let line = AuditRecord::Connection(ConnectionRecord::suppressed_summary(9, Some(&sb()), 12))
        .to_line()
        .unwrap();
    assert!(line.contains(r#""host":null"#), "{line}");
    assert!(line.contains(r#""reason":"suppressed""#));
    assert!(line.contains(r#""count":12"#));
}

#[test]
fn r23_control_characters_come_out_escaped() {
    let mut e = event();
    e.binding_id = Some("a\u{1b}[31m\u{7f}\u{9b}\u{2028}\n\"\\z".into());
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e))
        .to_line()
        .unwrap();
    assert!(!line.chars().any(char::is_control), "{line:?}");
    assert!(!line.contains('\u{2028}'));
    assert!(
        line.contains(r#"a\u001b[31m\u007f\u009b\u2028\n\"\\z"#),
        "{line}"
    );
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["binding_id"], e.binding_id.unwrap());
}

#[test]
fn r24_record_types_and_connection_fields() {
    let kinds: Vec<_> = every_record().iter().map(AuditRecord::kind).collect();
    for kind in [
        "connection",
        "pending_created",
        "pending_decided",
        "pending_expired",
        "pending_suppressed",
        "rule_created",
        "rule_updated",
        "rule_deleted",
        "rule_expired",
        "audit_trimmed",
    ] {
        assert!(kinds.contains(&kind), "{kind}");
    }
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &event()))
        .to_line()
        .unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    let fields: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    for field in [
        "type",
        "ts",
        "workspace_id",
        "host",
        "port",
        "resolved_ip",
        "upstream",
        "decision",
        "outcome",
        "reason",
        "rule_id",
        "pending_id",
        "binding_id",
        "injected",
        "unswapped_stand_ins",
        "scrubbed_stand_ins",
        "foreign_credentials",
        "answer_credentials",
        "refused_address",
        "refused_reason",
        "category",
        "signal",
        "method",
        "path",
        "path_truncated",
        "bytes_up",
        "bytes_down",
        "count",
    ] {
        assert!(fields.iter().any(|f| f == field), "{field}");
    }
    assert_eq!(value["decision"], "allow");
    assert_eq!(value["path"], "/v1/items");
}

#[test]
fn a_company_network_destination_is_recorded_with_its_category_and_signal() {
    let mut e = event();
    e.decision = ConnectionDecision::Blocked;
    e.reason = ConnectionReason::Blocked(BlockReason::CompanyNetwork(CompanySignal::VpnRoute));
    e.company_network = Some(CompanySignal::VpnRoute);
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e))
        .to_line()
        .unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["reason"], "toggle:company_network");
    assert_eq!(value["category"], "company_network");
    assert_eq!(value["signal"], "vpn_route");
    // Any other connection has neither field set.
    let plain = ConnectionRecord::from_event(1, &event());
    assert_eq!((plain.category, plain.signal), (None, None));
}

#[test]
fn r24_reason_and_decision_reach_the_record_as_codes() {
    let mut e = event();
    e.decision = ConnectionDecision::Blocked;
    e.reason = ConnectionReason::Blocked(BlockReason::LocalToggle(LocalCategory::Private));
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e))
        .to_line()
        .unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["decision"], "blocked");
    assert_eq!(value["reason"], "toggle:private");
}

#[test]
fn r25_no_secrets_in_connection_records() {
    let mut e = event();
    e.http = Some(HttpRequestLine::new(
        "POST",
        &format!("https://user:{CANARY}@api.example.com/login?token={CANARY}#{CANARY}"),
    ));
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e))
        .to_line()
        .unwrap();
    assert!(!line.contains(CANARY), "{line}");
    assert!(line.contains(r#""path":"/login""#), "{line}");
}

#[test]
fn r25_query_string_is_dropped_even_from_a_hand_built_record() {
    let mut record = ConnectionRecord::from_event(1, &event());
    record.path = Some(format!("/a?secret={CANARY}"));
    let line = AuditRecord::Connection(record).to_line().unwrap();
    assert!(!line.contains(CANARY), "{line}");
}

#[test]
fn r25_every_record_type_carries_no_secret_fields() {
    // Credential material has no field to go in; this pins that no record grows one.
    for record in every_record() {
        let line = record.to_line().unwrap();
        for word in [
            "header",
            "authorization",
            "cookie",
            "body",
            "query",
            "token",
            "secret",
        ] {
            assert!(!line.contains(word), "{word} in {line}");
        }
    }
}

#[test]
fn r26_path_cut_to_1kib_and_flagged() {
    let mut e = event();
    e.http = Some(HttpRequestLine::new(
        "GET",
        &format!("/{}", "é".repeat(2000)),
    ));
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e))
        .to_line()
        .unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert!(value["path"].as_str().unwrap().len() <= MAX_FIELD_BYTES);
    assert_eq!(value["path_truncated"], true);
}

#[test]
fn r26_line_never_exceeds_4kib_even_with_escapes() {
    let mut e = event();
    let nasty = "\u{1}".repeat(1024);
    e.binding_id = Some(nasty.clone());
    e.http = Some(HttpRequestLine::new(nasty.clone(), &format!("/{nasty}")));
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e))
        .to_line()
        .unwrap();
    assert!(line.len() <= MAX_LINE_BYTES, "{}", line.len());
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["path_truncated"], true);
}

/// `to_line` under a hang guard. The shortening loop ends only because every pass cuts a string; a
/// pass that cuts nothing would spin for ever, which a mutation run would see as a hang instead of
/// a failed test.
fn to_line_bounded(record: AuditRecord) -> Result<String, AuditError> {
    puddle_test_kit::returns_within(
        "to_line: its shortening loop must always end",
        std::time::Duration::from_secs(5),
        move || record.to_line(),
    )
}

#[test]
fn a_record_too_long_without_its_strings_is_refused() {
    let record = AuditRecord::RuleSetChanged {
        ts: 1,
        set_id: "builtin:github".into(),
        added: vec![String::new(); 2_000],
        removed: Vec::new(),
    };
    assert!(matches!(to_line_bounded(record), Err(AuditError::TooLarge)));
}

#[test]
fn long_strings_in_a_list_are_shortened_until_the_line_fits() {
    let record = AuditRecord::RuleSetChanged {
        ts: 1,
        set_id: "builtin:github".into(),
        added: vec!["a".repeat(1_000); 6],
        removed: vec!["b".into()],
    };
    let line = to_line_bounded(record).unwrap();
    assert!(line.len() <= MAX_LINE_BYTES, "{}", line.len());
    let value: Value = serde_json::from_str(&line).unwrap();
    let lengths: Vec<usize> = value["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().len())
        .collect();
    assert_eq!(lengths, [1_000, 500, 500, 500, 500, 500]);
    assert_eq!(value["removed"], json!(["b"]));
}

#[test]
fn a_string_of_exactly_the_field_limit_is_kept_and_not_flagged() {
    let mut e = event();
    e.http = Some(HttpRequestLine::new(
        "GET",
        &format!("/{}", "a".repeat(MAX_FIELD_BYTES - 1)),
    ));
    let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e))
        .to_line()
        .unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["path"].as_str().unwrap().len(), MAX_FIELD_BYTES);
    assert_eq!(value["path_truncated"], false);
}

#[test]
fn a_string_in_a_list_over_the_field_limit_is_cut_to_it() {
    let record = AuditRecord::RuleSetChanged {
        ts: 1,
        set_id: "builtin:github".into(),
        added: vec!["a".repeat(MAX_FIELD_BYTES + 1), "b".repeat(MAX_FIELD_BYTES)],
        removed: Vec::new(),
    };
    let value: Value = serde_json::from_str(&record.to_line().unwrap()).unwrap();
    assert_eq!(value["added"][0].as_str().unwrap().len(), MAX_FIELD_BYTES);
    assert_eq!(value["added"][1].as_str().unwrap().len(), MAX_FIELD_BYTES);
}

#[test]
fn a_line_of_exactly_the_limit_is_written_whole() {
    let with = |strings: &[usize]| AuditRecord::RuleSetChanged {
        ts: 1,
        set_id: "builtin:github".into(),
        added: strings.iter().map(|n| "a".repeat(*n)).collect(),
        removed: Vec::new(),
    };
    let base = to_line_bounded(with(&[0; 4])).unwrap().len();
    let spare = MAX_LINE_BYTES - base;
    let sizes = [spare / 4, spare / 4, spare / 4, spare - 3 * (spare / 4)];
    assert!(sizes.iter().all(|n| *n <= MAX_FIELD_BYTES), "{sizes:?}");
    let line = to_line_bounded(with(&sizes)).unwrap();
    assert_eq!(line.len(), MAX_LINE_BYTES);
    let value: Value = serde_json::from_str(&line).unwrap();
    let lengths: Vec<usize> = value["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap().len())
        .collect();
    assert_eq!(lengths, sizes, "nothing was cut");
}

#[test]
fn a_character_the_line_escapes_counts_as_six_bytes_when_choosing_what_to_cut() {
    for c in ['\u{1}', '\u{7f}', '"', '\\', '\u{2028}', '\u{2029}'] {
        assert!(needs_escape(c), "{c:?}");
        assert_eq!(encoded_len(&c.to_string()), 6, "{c:?}");
    }
    for c in ['a', 'é', '/', ' ', '\''] {
        assert!(!needs_escape(c), "{c:?}");
        assert_eq!(encoded_len(&c.to_string()), c.len_utf8(), "{c:?}");
    }
}

#[test]
fn connection_window_counts_excess_per_second() {
    let mut w = ConnectionWindow::default();
    assert_eq!(w.admit(5_000, 2), (true, None));
    assert_eq!(w.admit(5_100, 2), (true, None));
    assert_eq!(w.admit(5_200, 2), (false, None));
    assert_eq!(w.admit(5_999, 2), (false, None));
    assert_eq!(w.admit(6_000, 2), (true, Some((5_000, 2))));
    assert_eq!(w.roll(6_500), None);
    assert_eq!(w.roll(9_000), None);
}

#[test]
fn a_summary_whose_write_failed_is_handed_out_again_merged_with_the_next_one() {
    let mut w = ConnectionWindow::default();
    assert_eq!(w.admit(5_000, 1), (true, None));
    assert_eq!(w.admit(5_100, 1), (false, None));
    // The second ended, its summary was taken and its write failed twice.
    assert_eq!(w.roll(6_000), Some((5_000, 1)));
    w.owe(5_000, 1);
    w.owe(4_000, 2);
    assert_eq!(
        w.roll(6_100),
        Some((4_000, 3)),
        "owed counts add up, oldest second"
    );
    assert_eq!(w.roll(6_200), None, "handed out once");
    // Owed while the next second also had excess: both come out as one summary.
    assert_eq!(w.admit(6_300, 1), (true, None));
    assert_eq!(w.admit(6_400, 1), (false, None));
    w.owe(5_000, 4);
    assert_eq!(w.roll(7_000), Some((5_000, 5)));
}

#[test]
fn actor_strings() {
    assert_eq!(actor_str(Actor::Ui), "ui");
}

proptest! {
    #[test]
    fn r26_any_strings_give_a_parseable_line_within_the_cap(
        binding in ".{0,1500}",
        target in ".{0,3000}",
        method in ".{0,1500}",
    ) {
        let mut e = event();
        e.binding_id = Some(binding);
        e.http = Some(HttpRequestLine::new(method, &target));
        let line = AuditRecord::Connection(ConnectionRecord::from_event(1, &e)).to_line().unwrap();
        prop_assert!(line.len() <= MAX_LINE_BYTES);
        prop_assert!(!line.chars().any(char::is_control));
        let value: Value = serde_json::from_str(&line).unwrap();
        prop_assert_eq!(&value["type"], "connection");
        prop_assert!(!value["path"].as_str().unwrap().contains('?'));
    }
}

#[test]
fn kinds_lists_every_type_tag_once() {
    let kinds: Vec<&str> = every_record().iter().map(AuditRecord::kind).collect();
    for kind in kinds {
        assert!(AuditRecord::KINDS.contains(&kind), "{kind}");
    }
    let mut sorted = AuditRecord::KINDS.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), AuditRecord::KINDS.len());
}

fn workspace_op(outcome: WorkspaceOpOutcome, detail: Option<&str>) -> AuditRecord {
    AuditRecord::WorkspaceOp {
        ts: 7,
        op: WorkspaceOpKind::Delete,
        workspace_id: Some("acme".into()),
        outcome,
        actor: "api".into(),
        detail: detail.map(str::to_owned),
    }
}

#[test]
fn a_workspace_op_has_a_workspace_an_outcome_and_no_host() {
    let ok = workspace_op(WorkspaceOpOutcome::Succeeded, None);
    let failed = workspace_op(WorkspaceOpOutcome::Failed, Some("the volume is busy"));
    assert_eq!(ok.kind(), "workspace_op");
    assert!(AuditRecord::KINDS.contains(&ok.kind()));
    assert_eq!(
        (ok.workspace_id(), ok.host(), ok.origin(), ok.ts()),
        (Some("acme"), None, None, 7)
    );
    assert_eq!(ok.outcome(), Some(AuditOutcome::Succeeded));
    assert_eq!(failed.outcome(), Some(AuditOutcome::Failed));
    let identity = AuditRecord::WorkspaceOp {
        ts: 7,
        op: WorkspaceOpKind::IdentityCreated,
        workspace_id: None,
        outcome: WorkspaceOpOutcome::Succeeded,
        actor: "api".into(),
        detail: Some("Work".into()),
    };
    assert_eq!(identity.workspace_id(), None);
}

#[test]
fn a_workspace_op_line_round_trips_and_one_without_a_detail_reads() {
    let failed = workspace_op(WorkspaceOpOutcome::Failed, Some("the volume is busy"));
    let line = failed.to_line().unwrap();
    let value: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(value["type"], "workspace_op");
    assert_eq!(value["op"], "delete");
    assert_eq!(value["outcome"], "failed");
    assert_eq!(serde_json::from_str::<AuditRecord>(&line).unwrap(), failed);
    // A line with no detail field (the field is optional) reads as none, and is not written.
    let ok = workspace_op(WorkspaceOpOutcome::Succeeded, None);
    let line = ok.to_line().unwrap();
    assert!(!line.contains("detail"), "{line}");
    assert_eq!(serde_json::from_str::<AuditRecord>(&line).unwrap(), ok);
}

#[test]
fn host_and_outcome_per_record_kind() {
    let mut denied = pending_wire();
    denied.state = "denied".into();
    let mut allowed = pending_wire();
    allowed.state = "allowed".into();
    let decided = |pending| AuditRecord::PendingDecided { ts: 1, pending };
    assert_eq!(decided(allowed).outcome(), Some(AuditOutcome::Allow));
    assert_eq!(decided(denied).outcome(), Some(AuditOutcome::Deny));
    assert_eq!(decided(pending_wire()).outcome(), None);
    let mut upper = rule_wire();
    upper.pattern = ".Example.COM".into();
    let created = AuditRecord::RuleCreated { ts: 1, rule: upper };
    assert_eq!(created.host().as_deref(), Some(".example.com"));
    assert_eq!(created.outcome(), None);
    let summary = AuditRecord::Connection(ConnectionRecord::suppressed_summary(1, Some(&sb()), 5));
    assert_eq!((summary.host(), summary.outcome()), (None, None));
    for decision in [
        ConnectionDecision::Allow,
        ConnectionDecision::Deny,
        ConnectionDecision::Pending,
        ConnectionDecision::Blocked,
    ] {
        let mut record = ConnectionRecord::from_event(1, &event());
        record.decision = Some(decision);
        let outcome = AuditRecord::Connection(record).outcome().unwrap();
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            serde_json::to_value(decision).unwrap()
        );
    }
    for outcome in AuditOutcome::ALL {
        assert_eq!(
            serde_json::to_value(outcome).unwrap(),
            outcome.as_str(),
            "{outcome:?}"
        );
    }
}

type Row = (i64, String, Option<String>, Option<String>, Option<String>);

#[test]
fn migration_backfills_host_outcome_and_search_text_as_the_code_derives_them() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::schema::migrate_up_to(&mut conn, 1).unwrap();
    let mut records = every_record();
    // Upper-case text in every searched field, and empty or missing ones.
    let mut shouting = ConnectionRecord::from_event(9, &event());
    shouting.host = Some("API.Example.COM".into());
    shouting.method = Some("PoSt".into());
    shouting.path = Some("/V1/Ítems".into());
    records.push(AuditRecord::Connection(shouting.clone()));
    shouting.path = Some(String::new());
    shouting.host = None;
    records.push(AuditRecord::Connection(shouting));
    let mut allowed = pending_wire();
    allowed.state = "allowed".into();
    let mut denied = pending_wire();
    denied.state = "denied".into();
    records.extend([
        AuditRecord::PendingDecided {
            ts: 9,
            pending: allowed,
        },
        AuditRecord::PendingDecided {
            ts: 9,
            pending: denied,
        },
    ]);
    // The database as version 1 left it: the owner column still has its old name. Each row's id
    // is its key: the check below finds a record by it, not by where the row comes back.
    let mut by_id = std::collections::HashMap::new();
    for record in &records {
        conn.execute(
            "INSERT INTO audit (ts, type, sandbox_id, line) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                i64::try_from(record.ts()).unwrap(),
                record.kind(),
                record.workspace_id(),
                record.to_line().unwrap()
            ],
        )
        .unwrap();
        by_id.insert(conn.last_insert_rowid(), record);
    }
    crate::schema::migrate(&mut conn).unwrap();
    let mut stmt = conn
        .prepare("SELECT id, type, host, outcome, search FROM audit")
        .unwrap();
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(rows.len(), records.len());
    for (id, kind, host, outcome, search) in rows {
        let record = by_id
            .get(&id)
            .unwrap_or_else(|| panic!("the migration made up a row {id}"));
        assert_eq!(kind, record.kind());
        assert_eq!(host, record.host(), "{kind} host");
        assert_eq!(
            outcome.as_deref(),
            record.outcome().map(AuditOutcome::as_str),
            "{kind} outcome"
        );
        assert_eq!(search, record.search_text(), "{kind} search text");
    }
}

#[test]
fn search_text_is_the_lower_case_fields_a_person_would_type() {
    let mut connection = ConnectionRecord::from_event(9, &event());
    connection.method = Some("GET".into());
    connection.path = Some("/V1/Items".into());
    assert_eq!(
        AuditRecord::Connection(connection).search_text().as_deref(),
        Some("api.example.com\nget\n/v1/items\nrule")
    );
    let mut company = ConnectionRecord::from_event(9, &event());
    company.category = Some("company_network".into());
    assert_eq!(
        AuditRecord::Connection(company).search_text().as_deref(),
        Some("api.example.com\nget\n/v1/items\nrule\ncompany_network")
    );
    let summary = AuditRecord::Connection(ConnectionRecord::suppressed_summary(1, Some(&sb()), 5));
    assert_eq!(summary.search_text().as_deref(), Some("suppressed"));
    let mut rule = rule_wire();
    rule.pattern = ".Example.COM".into();
    assert_eq!(
        AuditRecord::RuleCreated { ts: 1, rule }
            .search_text()
            .as_deref(),
        Some(".example.com")
    );
    let nothing = AuditRecord::PendingSuppressed {
        ts: 1,
        workspace_id: "sb-1".into(),
        count: 1,
    };
    assert_eq!(nothing.search_text(), None);
}
