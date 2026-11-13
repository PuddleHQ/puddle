// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn pending_outcome_exposes_row_id() {
    assert_eq!(
        PendingOutcome::New(PendingId(3)).pending_id(),
        Some(PendingId(3))
    );
    assert_eq!(
        PendingOutcome::Repeat(PendingId(4)).pending_id(),
        Some(PendingId(4))
    );
    assert_eq!(PendingOutcome::Unlisted.pending_id(), None);
}

#[test]
fn block_reasons_have_their_audit_codes() {
    assert_eq!(BlockReason::SshUnsupported.to_string(), "ssh_unsupported");
    assert_eq!(BlockReason::UdpUnsupported.to_string(), "udp_unsupported");
    assert_eq!(BlockReason::PuddleEndpoint.code(), "puddle_endpoint");
    assert_eq!(BlockReason::LocalAddress.code(), "local_address");
    assert_eq!(
        BlockReason::SettingsUnreadable.code(),
        "settings_unreadable"
    );
    assert_eq!(
        BlockReason::LocalToggle(LocalCategory::LinkLocal).to_string(),
        "toggle:link_local"
    );
}

#[test]
fn a_company_network_block_names_its_toggle_like_the_other_switches() {
    for signal in CompanySignal::ALL {
        let reason = BlockReason::CompanyNetwork(signal);
        assert_eq!(reason.code(), "toggle:company_network");
        assert_eq!(reason.to_string(), "toggle:company_network");
    }
}

#[test]
fn set_decisions_name_their_set_and_only_set_allows_let_through() {
    let allow = Decision::SetAllow {
        set: RuleSetId::System,
        rule_id: None,
        pattern: PatternKind::Exact,
    };
    let deny = Decision::SetDeny {
        set: RuleSetId::User(4),
        rule_id: Some(RuleId(9)),
        pattern: PatternKind::Suffix,
    };
    assert!(allow.is_allow());
    assert!(!deny.is_allow());
    assert_eq!(allow.rule_set(), Some(RuleSetId::System));
    assert_eq!(deny.rule_set(), Some(RuleSetId::User(4)));
    let own = Decision::Allow {
        rule_id: RuleId(1),
        pattern: PatternKind::Exact,
    };
    assert_eq!(own.rule_set(), None);
    assert!(RuleSetId::System < RuleSetId::BuiltIn("a"));
    assert!(RuleSetId::BuiltIn("z") < RuleSetId::User(0));
}

#[test]
fn only_allow_lets_a_connection_through() {
    let allow = Decision::Allow {
        rule_id: RuleId(1),
        pattern: PatternKind::Exact,
    };
    assert!(allow.is_allow());
    assert!(
        !Decision::Blocked {
            reason: BlockReason::LocalAddress
        }
        .is_allow()
    );
    assert!(!Decision::Pending(PendingOutcome::Unlisted).is_allow());
}

#[test]
fn requests_carry_an_optional_protocol_hint() {
    let workspace = WorkspaceId::new("box").unwrap();
    let host = Host::parse_normalised("example.com").unwrap();
    let plain = EgressRequest::new(workspace, host, 443);
    assert_eq!(plain.protocol, None);
    let ssh = plain.clone().with_protocol(ProtocolHint::Ssh);
    assert_eq!(ssh.protocol, Some(ProtocolHint::Ssh));
    assert_eq!(
        (ssh.port, ssh.host.to_string()),
        (443, "example.com".into())
    );
}

#[test]
fn rule_sets_display_as_their_stored_names() {
    assert_eq!(RuleSetId::System.to_string(), "system");
    assert_eq!(RuleSetId::BuiltIn("npm").to_string(), "builtin:npm");
    assert_eq!(RuleSetId::User(12).to_string(), "user:12");
}

struct AlwaysBlocked;

impl Policy for AlwaysBlocked {
    fn decide(&self, _: &EgressRequest, _: SuffixAllows) -> Result<Decision, PolicyError> {
        Ok(Decision::Blocked {
            reason: BlockReason::LocalAddress,
        })
    }
}

#[test]
fn a_policy_without_ip_rules_finds_no_rule_on_lookup() {
    let request = EgressRequest::new(
        WorkspaceId::new("box").unwrap(),
        Host::parse_normalised("example.com").unwrap(),
        443,
    );
    for suffix in [SuffixAllows::Count, SuffixAllows::Ignore] {
        assert_eq!(
            AlwaysBlocked.decide(&request, suffix),
            Ok(Decision::Blocked {
                reason: BlockReason::LocalAddress
            })
        );
        assert_eq!(AlwaysBlocked.lookup(&request, suffix), Ok(None));
    }
}

#[test]
fn ids_and_errors_display() {
    assert_eq!(RuleId(7).to_string(), "7");
    assert_eq!(PendingId(8).to_string(), "8");
    let err = PolicyError {
        reason: "database is locked".into(),
    };
    assert_eq!(err.to_string(), "policy unavailable: database is locked");
}
