// SPDX-License-Identifier: GPL-3.0-or-later
use std::io;

use super::*;

fn blob(name: &str) -> Vec<u8> {
    let mut v = u32::try_from(name.len()).unwrap().to_be_bytes().to_vec();
    v.extend_from_slice(name.as_bytes());
    v
}

fn key(name: &str) -> Identity {
    Identity::new(blob(name), "a comment that must not be reported".into())
}

fn facts(endpoint: &str, answer: Result<Vec<Identity>, AgentError>) -> SshAgentFacts {
    facts_from_answer(endpoint.into(), answer, || Some(AgentService::Stopped))
}

#[test]
fn keys_are_reported_as_families_only() {
    let got = facts(
        "/run/agent.sock",
        Ok(vec![
            key("ssh-ed25519"),
            key("ssh-rsa-cert-v01@openssh.com"),
            Identity::new(vec![0, 0], String::new()),
            key("made-up@example.com"),
        ]),
    );
    assert_eq!(
        got,
        SshAgentFacts::Keys {
            endpoint: "/run/agent.sock".into(),
            families: ["ed25519", "rsa", "other", "other"]
                .map(String::from)
                .to_vec(),
        }
    );
}

#[test]
fn the_service_is_read_only_for_the_default_pipe() {
    let gone = || AgentError::Unreachable(io::ErrorKind::NotFound.into());
    let named = facts(r"\\.\pipe\pageant.alice", Err(gone()));
    assert!(
        matches!(named, SshAgentFacts::Unreachable { service: None, .. }),
        "{named:?}"
    );
    let default = facts(WINDOWS_DEFAULT_PIPE, Err(gone()));
    assert!(
        format!("{default:?}").contains("service: Some(Stopped)"),
        "{default:?}"
    );
}

#[test]
fn each_error_has_its_own_facts() {
    let at = |e| facts("/s", Err(e));
    assert_eq!(
        at(AgentError::Refused),
        SshAgentFacts::Locked {
            endpoint: "/s".into()
        }
    );
    let limit = Duration::from_secs(2);
    assert_eq!(
        at(AgentError::TimedOut(limit)),
        SshAgentFacts::TimedOut {
            endpoint: "/s".into(),
            limit
        }
    );
    for e in [
        AgentError::Io(io::ErrorKind::BrokenPipe.into()),
        AgentError::Protocol("not an identities answer"),
    ] {
        let text = e.to_string();
        assert_eq!(
            at(e),
            SshAgentFacts::Unusable {
                endpoint: "/s".into(),
                detail: text
            }
        );
    }
}

#[test]
fn no_endpoint_asks_nothing() {
    let got = ssh_agent_facts(None, Duration::from_secs(2), || None);
    assert_eq!(got, SshAgentFacts::NoEndpoint);
}

#[test]
fn a_probe_with_nothing_at_the_named_endpoint_says_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-agent");
    let probe = SystemProbe::new(
        dir.path().to_path_buf(),
        RuntimeVersion::built_for(),
        DevOverride::from_env(),
    )
    .with_ssh_agent_pipe(Some(missing.display().to_string()));
    let got = probe.ssh_agent(Duration::from_secs(2));
    assert!(
        matches!(&got, SshAgentFacts::Unreachable { endpoint, service: None, by_default: false, .. } if *endpoint == missing.display().to_string()),
        "{got:?}"
    );
}

#[test]
fn the_version_is_the_first_line_of_az_version() {
    assert_eq!(
        azure_cli_version("azure-cli                         2.64.0 *\n\ncore 2.64.0 *\n"),
        Some("2.64.0".to_owned())
    );
    assert_eq!(azure_cli_version("core 2.64.0 *\n"), None);
    assert_eq!(azure_cli_version(""), None);
}
