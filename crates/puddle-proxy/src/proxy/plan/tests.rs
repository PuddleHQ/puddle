// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;

use puddle_ca::CaBuilder;
use puddle_netpolicy::normalise_host;
use puddle_types::{
    ConnectionDecision, ConnectionReason, EgressRequest, Host, Methods, NullSink, PassThroughCode,
    PathPattern, ProtocolHint, ReasonCode, RuleId, WorkspaceId,
};

use super::{Admitted, ConnectionPlan, GuestHints, Seen};
use crate::Proxy;
use crate::http::Head;
use crate::terminate::{
    Interception, Mode, NoInjection, RequestRule, RequestRules, Termination, TerminationSet,
    Terminations,
};
use crate::testing::StaticPolicy;

fn workspace() -> WorkspaceId {
    WorkspaceId::new("ws").unwrap()
}

fn name(h: &str) -> Host {
    normalise_host(h).unwrap().into_host()
}

/// A `CONNECT` head, with the agent's container mark when `from_container`.
fn head(from_container: bool) -> Head {
    Head {
        method: "CONNECT".into(),
        uri: "x:443".into(),
        version: "HTTP/1.1".into(),
        headers: if from_container {
            vec!["x-puddle-origin: container".into()]
        } else {
            vec![]
        },
    }
}

/// A proxy whose workspace decrypts `bound` and, with `everything`, every other name; the rules on
/// method and path cover `ruled.example`.
fn proxy(bound: &[&str], everything: bool) -> Proxy {
    let ca = Arc::new(CaBuilder::new("test").build().unwrap());
    let rules = RequestRules::new(vec![
        RequestRule::new(
            RuleId(7),
            "ruled.example",
            Methods::parse(["POST"]).unwrap(),
            PathPattern::parse("/admin").unwrap(),
        )
        .unwrap(),
    ]);
    let interception = Interception::new(Arc::default()).decrypt_all(everything);
    let termination = Termination::new(
        TerminationSet::parse(bound.iter().copied()).unwrap(),
        ca,
        Arc::new(NoInjection),
    )
    .with_interception(interception)
    .with_request_rules(rules);
    let terminations = Arc::new(Terminations::new());
    terminations.insert(workspace(), termination);
    Proxy::new(Arc::new(StaticPolicy::new()), Arc::new(NullSink))
        .with_termination(terminations, puddle_upstream::TlsClient::new([]).unwrap())
}

fn request(h: &str, port: u16, ssh: bool) -> EgressRequest {
    let request = EgressRequest::new(workspace(), name(h), port);
    if ssh {
        request.with_protocol(ProtocolHint::Ssh)
    } else {
        request
    }
}

fn plan<'p>(proxy: &'p Proxy, request: &EgressRequest, from_container: bool) -> ConnectionPlan<'p> {
    let head = head(from_container);
    let seen = proxy.seen(&request.workspace);
    ConnectionPlan::new(proxy, &seen, request, &head, GuestHints::of(request, &head))
}

/// What a plan says, short enough to compare.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    NotRunning,
    GitSsh,
    SshRefused,
    RuleRefusesPort,
    Terminate(Mode, bool),
    Splice(Option<PassThroughCode>, bool),
}

fn shape(plan: &ConnectionPlan<'_>) -> Shape {
    match plan {
        ConnectionPlan::NotRunning => Shape::NotRunning,
        ConnectionPlan::GitSsh { .. } => Shape::GitSsh,
        ConnectionPlan::SshRefused => Shape::SshRefused,
        ConnectionPlan::Admit(Admitted::RuleRefusesPort(..)) => Shape::RuleRefusesPort,
        ConnectionPlan::Admit(Admitted::Terminate {
            mode, ssh_first, ..
        }) => Shape::Terminate(*mode, *ssh_first),
        ConnectionPlan::Admit(Admitted::Splice(splice)) => {
            Shape::Splice(splice.passed_through, splice.git_ssh_name)
        }
    }
}

#[test]
fn the_registry_is_read_into_one_of_three_states() {
    let plain = Proxy::new(Arc::new(StaticPolicy::new()), Arc::new(NullSink));
    assert!(matches!(plain.seen(&workspace()), Seen::Unterminated));
    let proxy = proxy(&["bound.example"], false);
    assert!(matches!(proxy.seen(&workspace()), Seen::Running(_)));
    let other = WorkspaceId::new("other").unwrap();
    assert!(matches!(proxy.seen(&other), Seen::NotRunning));
}

/// A guest hint never undoes the decryption a credential, secret or rule asks for.
#[test]
fn no_hint_moves_a_bound_or_ruled_host_off_decryption() {
    let proxy = proxy(&["bound.example", "ruled.example"], true);
    for h in ["bound.example", "ruled.example"] {
        for from_container in [false, true] {
            assert_eq!(
                shape(&plan(&proxy, &request(h, 443, false), from_container)),
                Shape::Terminate(Mode::Bound, false),
                "{h} from_container={from_container}"
            );
            // Announcing SSH only lets the first bytes decide; it does not splice by itself.
            assert_eq!(
                shape(&plan(&proxy, &request(h, 443, true), from_container)),
                Shape::Terminate(Mode::Bound, true),
                "{h} ssh from_container={from_container}"
            );
        }
    }
}

#[test]
fn a_container_mark_only_passes_through_a_host_decrypted_for_every_host() {
    let proxy = proxy(&["bound.example"], true);
    let req = request("other.example", 443, false);
    assert_eq!(
        shape(&plan(&proxy, &req, false)),
        Shape::Terminate(Mode::Everything, false)
    );
    assert_eq!(
        shape(&plan(&proxy, &req, true)),
        Shape::Splice(Some(PassThroughCode::Container), false)
    );
}

#[test]
fn a_tunnel_to_another_port_of_a_ruled_host_is_refused_after_admission() {
    let proxy = proxy(&["ruled.example"], false);
    assert_eq!(
        shape(&plan(&proxy, &request("ruled.example", 8443, false), false)),
        Shape::RuleRefusesPort
    );
    assert_eq!(
        shape(&plan(&proxy, &request("free.example", 8443, false), false)),
        Shape::Splice(None, false)
    );
}

#[test]
fn ssh_to_a_git_name_puddle_does_not_serve_is_refused_and_other_ssh_is_spliced() {
    // No `with_git_ssh`: Puddle serves no SSH, so a built-in Git host's SSH is refused.
    let proxy = proxy(&[], false);
    assert_eq!(
        shape(&plan(&proxy, &request("github.com", 22, true), false)),
        Shape::SshRefused
    );
    assert_eq!(
        shape(&plan(&proxy, &request("shell.example", 22, true), false)),
        Shape::Splice(None, false)
    );
    // Not announced as SSH: decided like any tunnel, and SSH found in it is refused there.
    assert_eq!(
        shape(&plan(&proxy, &request("github.com", 22, false), false)),
        Shape::Splice(None, true)
    );
}

#[test]
fn a_workspace_without_a_termination_is_refused_whatever_the_request() {
    let proxy = proxy(&["ruled.example"], false);
    let other = WorkspaceId::new("other").unwrap();
    for (h, port, ssh) in [
        ("ruled.example", 8443, false),
        ("ruled.example", 443, false),
        ("free.example", 443, false),
        ("github.com", 22, true),
    ] {
        let req = EgressRequest::new(other.clone(), name(h), port);
        let req = if ssh {
            req.with_protocol(ProtocolHint::Ssh)
        } else {
            req
        };
        assert_eq!(
            shape(&plan(&proxy, &req, false)),
            Shape::NotRunning,
            "{h}:{port}"
        );
    }
}

#[test]
fn a_proxy_without_terminations_still_splices() {
    let plain = Proxy::new(Arc::new(StaticPolicy::new()), Arc::new(NullSink));
    assert_eq!(
        shape(&plan(&plain, &request("free.example", 8443, false), false)),
        Shape::Splice(None, false)
    );
}

#[test]
fn the_not_running_refusal_is_a_503_recorded_as_blocked() {
    let req = request("free.example", 443, false);
    let (refusal, event) = crate::proxy::refuse_not_running(&req);
    assert_eq!(refusal.status, "503 Service Unavailable");
    assert_eq!(refusal.code, Some(ReasonCode::WorkspaceNotRunning));
    assert!(
        refusal
            .headers
            .contains(&("x-puddle-blocked", "workspace_not_running".to_owned()))
    );
    assert_eq!(event.decision, ConnectionDecision::Blocked);
    assert_eq!(
        event.reason,
        ConnectionReason::Refused(ReasonCode::WorkspaceNotRunning)
    );
}
