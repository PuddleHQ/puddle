// SPDX-License-Identifier: GPL-3.0-or-later
#[test]
fn a_refused_reason_has_the_code_the_audit_records() {
    use crate::CompanySignal;
    use crate::policy::BlockReason;
    let guard = |r: BlockReason| RefusedReason::from(r).code();
    assert_eq!(RefusedReason::IpRule.code(), "ip_rule");
    assert_eq!(guard(BlockReason::SshUnsupported), "ssh_unsupported");
    assert_eq!(guard(BlockReason::UdpUnsupported), "udp_unsupported");
    assert_eq!(guard(BlockReason::PuddleEndpoint), "puddle_endpoint");
    assert_eq!(guard(BlockReason::LocalAddress), "local_address");
    assert_eq!(
        guard(BlockReason::SettingsUnreadable),
        "settings_unreadable"
    );
    for category in LocalCategory::ALL {
        let code = guard(BlockReason::LocalToggle(category));
        assert_eq!(code, category.audit_reason());
        assert!(code.starts_with("toggle:"), "{code}");
    }
    assert_eq!(
        guard(BlockReason::CompanyNetwork(CompanySignal::PacDirect)),
        "toggle:company_network"
    );
}

#[test]
fn an_outcome_prints_as_its_wire_name() {
    assert_eq!(
        ConnectionOutcome::UpstreamRefused.to_string(),
        "upstream_refused"
    );
    assert_eq!(ConnectionOutcome::Timeout.to_string(), "timeout");
    assert_eq!(ConnectionOutcome::Connected.to_string(), "connected");
    assert_eq!(
        ConnectionOutcome::ConnectFailed.to_string(),
        "connect_failed"
    );
}

#[test]
fn an_origin_prints_as_its_wire_name() {
    assert_eq!(ConnectionOrigin::Workspace.as_str(), "workspace");
    assert_eq!(ConnectionOrigin::Puddle.to_string(), "puddle");
}

#[test]
fn a_failed_puddle_request_reason_carries_its_code() {
    assert_eq!(
        ConnectionReason::PuddleRequestFailed("bad_gateway").to_string(),
        "puddle_request_failed:bad_gateway"
    );
}

use super::*;
use crate::{LocalCategory, PatternKind, PendingOutcome};

const CANARY: &str = "CANARY-types-41c2";

fn request() -> EgressRequest {
    EgressRequest::new(
        WorkspaceId::new("box").unwrap(),
        Host::parse_normalised("api.example.com").unwrap(),
        443,
    )
}

#[test]
fn reasons_serialise_as_documented() {
    let reasons = [
        (ConnectionReason::Rule, "rule"),
        (ConnectionReason::NoRule, "no_rule"),
        (
            ConnectionReason::Blocked(BlockReason::LocalToggle(LocalCategory::Private)),
            "toggle:private",
        ),
        (
            ConnectionReason::Blocked(BlockReason::PuddleEndpoint),
            "puddle_endpoint",
        ),
        (
            ConnectionReason::Blocked(BlockReason::SshUnsupported),
            "ssh_unsupported",
        ),
        (
            ConnectionReason::Blocked(BlockReason::UdpUnsupported),
            "udp_unsupported",
        ),
        (
            ConnectionReason::Blocked(BlockReason::LocalAddress),
            "local_address",
        ),
        (ConnectionReason::PolicyUnavailable, "policy_unavailable"),
        (ConnectionReason::SniMismatch, "sni_mismatch"),
        (ConnectionReason::RequestRule, "request_rule"),
        (ConnectionReason::ConnectionLimit, "connection_limit"),
        (ConnectionReason::GuestTlsRejected, "guest_tls_rejected"),
        (
            ConnectionReason::CertificateUnavailable,
            "certificate_unavailable",
        ),
        (ConnectionReason::Suppressed, "suppressed"),
        (ConnectionReason::Merged, "merged"),
        (ConnectionReason::Refused("push_denied"), "push_denied"),
        (
            ConnectionReason::LimitEnded("stream_idle"),
            "limit:stream_idle",
        ),
        (
            ConnectionReason::PassedThrough("learned"),
            "pass_through:learned",
        ),
    ];
    for (reason, text) in reasons {
        assert_eq!(reason.to_string(), text);
    }
}

#[test]
fn decisions_serialise_snake_case() {
    let text = serde_json::to_string(&[
        ConnectionDecision::Allow,
        ConnectionDecision::Deny,
        ConnectionDecision::Pending,
        ConnectionDecision::Blocked,
    ])
    .unwrap();
    assert_eq!(text, r#"["allow","deny","pending","blocked"]"#);
}

#[test]
fn every_engine_decision_maps_to_its_record() {
    let r = request();
    let allow = ConnectionEvent::decided(
        &r,
        &Decision::Allow {
            rule_id: RuleId(3),
            pattern: PatternKind::Suffix,
        },
    );
    assert_eq!(
        (
            allow.decision,
            allow.reason,
            allow.rule_id,
            allow.pending_id
        ),
        (
            ConnectionDecision::Allow,
            ConnectionReason::Rule,
            Some(RuleId(3)),
            None
        )
    );
    let deny = ConnectionEvent::decided(
        &r,
        &Decision::Deny {
            rule_id: RuleId(4),
            pattern: PatternKind::Exact,
        },
    );
    assert_eq!(
        (deny.decision, deny.rule_id),
        (ConnectionDecision::Deny, Some(RuleId(4)))
    );
    assert_eq!(deny.rule_set, None);
    let by_system = ConnectionEvent::decided(
        &r,
        &Decision::SetAllow {
            set: RuleSetId::System,
            rule_id: None,
            pattern: PatternKind::Exact,
        },
    );
    assert_eq!(
        (by_system.decision, by_system.rule_id, by_system.rule_set),
        (ConnectionDecision::Allow, None, Some(RuleSetId::System))
    );
    let by_user_set = ConnectionEvent::decided(
        &r,
        &Decision::SetDeny {
            set: RuleSetId::User(2),
            rule_id: Some(RuleId(8)),
            pattern: PatternKind::Suffix,
        },
    );
    assert_eq!(
        (
            by_user_set.decision,
            by_user_set.reason,
            by_user_set.rule_id,
            by_user_set.rule_set
        ),
        (
            ConnectionDecision::Deny,
            ConnectionReason::Rule,
            Some(RuleId(8)),
            Some(RuleSetId::User(2))
        )
    );
    let unlisted = ConnectionEvent::decided(&r, &Decision::Pending(PendingOutcome::Unlisted));
    assert_eq!(
        (unlisted.decision, unlisted.pending_id),
        (ConnectionDecision::Pending, None)
    );
    let blocked = ConnectionEvent::decided(
        &r,
        &Decision::Blocked {
            reason: BlockReason::PuddleEndpoint,
        },
    );
    assert_eq!(
        (blocked.decision, blocked.reason, blocked.rule_id),
        (
            ConnectionDecision::Blocked,
            ConnectionReason::Blocked(BlockReason::PuddleEndpoint),
            None
        )
    );
    assert_eq!(
        (
            blocked.workspace.as_ref().map(WorkspaceId::as_str),
            blocked.host.to_string(),
            blocked.port
        ),
        (Some("box"), "api.example.com".to_owned(), 443)
    );
    assert_eq!(
        (blocked.bytes_up, blocked.bytes_down, blocked.injected),
        (0, 0, false)
    );
}

#[test]
fn a_request_line_never_keeps_query_fragment_or_userinfo() {
    let line = HttpRequestLine::new(
        "POST",
        &format!("http://u:{CANARY}@h.example/login?token={CANARY}#{CANARY}"),
    );
    assert_eq!((line.method(), line.path()), ("POST", "/login"));
    let line = HttpRequestLine::new("GET", &format!("/a/b?{CANARY}"));
    assert_eq!(line.path(), "/a/b");
    assert!(!format!("{line:?}").contains(CANARY));
}

#[test]
fn request_path_strips_everything_but_the_path() {
    assert_eq!(request_path("/a/b?c=d"), "/a/b");
    assert_eq!(request_path("/a#frag"), "/a");
    assert_eq!(request_path("http://h.example/x/y?z"), "/x/y");
    assert_eq!(request_path("http://u:p@h.example"), "/");
    assert_eq!(request_path("*"), "*");
    assert_eq!(request_path("/a://b/admin"), "/a://b/admin");
}

#[test]
fn the_certificate_distribution_exception_has_its_own_code() {
    assert_eq!(
        ConnectionReason::PkiDistributionPoint.to_string(),
        "pki_distribution_point"
    );
}

#[test]
fn puddle_events_have_an_origin_and_no_workspace() {
    let event = ConnectionEvent::puddle(
        Host::parse_normalised("registry.example").unwrap(),
        443,
        ConnectionDecision::Allow,
        ConnectionReason::PuddleRequest,
    );
    assert_eq!(event.origin, ConnectionOrigin::Puddle);
    assert_eq!(event.workspace, None);
    assert_eq!(event.reason.to_string(), "puddle_request");
    assert_eq!(
        ConnectionEvent::new(
            &request(),
            ConnectionDecision::Allow,
            ConnectionReason::Rule
        )
        .origin,
        ConnectionOrigin::Workspace
    );
    assert_eq!(ConnectionOrigin::default(), ConnectionOrigin::Workspace);
    assert_eq!(ConnectionOrigin::Puddle.to_string(), "puddle");
    assert_eq!(
        serde_json::to_string(&[ConnectionOrigin::Workspace, ConnectionOrigin::Puddle]).unwrap(),
        r#"["workspace","puddle"]"#
    );
}

#[derive(Default)]
struct Recorder(std::sync::Mutex<Vec<ConnectionEvent>>);

impl ConnectionLog for Recorder {
    fn record(&self, event: &ConnectionEvent) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[test]
fn a_shared_log_hands_every_event_on_to_the_log_it_wraps() {
    let recorder = std::sync::Arc::new(Recorder::default());
    let shared: std::sync::Arc<dyn ConnectionLog> = recorder.clone();
    let event = ConnectionEvent::new(
        &request(),
        ConnectionDecision::Deny,
        ConnectionReason::NoRule,
    );
    shared.record(&event);
    std::sync::Arc::clone(&recorder).record(&event);
    let seen = recorder.0.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[0], event);
}

#[test]
fn the_null_log_takes_events_through_a_shared_handle_and_leaves_them_alone() {
    let log: std::sync::Arc<dyn ConnectionLog> = std::sync::Arc::new(NullConnectionLog);
    let event = ConnectionEvent::new(
        &request(),
        ConnectionDecision::Deny,
        ConnectionReason::NoRule,
    );
    let before = event.clone();
    log.record(&event);
    assert_eq!(event, before);
}
