// SPDX-License-Identifier: AGPL-3.0-or-later
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
        // az-ok: only the SSH agent is probed here, never `az`
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

fn exited(code: Option<i32>, first_line: &str) -> ProcessOutcome {
    ProcessOutcome::Exited {
        code,
        stdout_first_line: first_line.into(),
        stderr_tail: String::new(),
        elapsed: Duration::from_millis(5),
    }
}

#[test]
fn a_clean_az_version_run_reports_the_version() {
    assert_eq!(
        az_cli_facts(exited(Some(0), "azure-cli  2.64.0 *")),
        AzCliFacts::Found {
            version: "2.64.0".into()
        }
    );
}

#[test]
fn an_az_that_prints_no_version_is_unknown() {
    assert_eq!(
        az_cli_facts(exited(Some(0), "something else")),
        AzCliFacts::Unknown {
            detail: "no version in its first line".into()
        }
    );
}

#[test]
fn an_az_that_fails_is_unknown_with_its_exit_code() {
    assert_eq!(
        az_cli_facts(exited(Some(3), "")),
        AzCliFacts::Unknown {
            detail: "`az --version` exited with Some(3)".into()
        }
    );
    assert_eq!(
        az_cli_facts(exited(None, "")),
        AzCliFacts::Unknown {
            detail: "`az --version` exited with None".into()
        }
    );
}

#[test]
fn an_az_that_hangs_is_unknown_with_the_limit() {
    assert_eq!(
        az_cli_facts(ProcessOutcome::TimedOut {
            after: Duration::from_secs(7)
        }),
        AzCliFacts::Unknown {
            detail: "`az --version` took more than 7 s".into()
        }
    );
}

#[test]
fn an_az_that_cannot_start_is_unknown_with_the_os_text() {
    assert_eq!(
        az_cli_facts(ProcessOutcome::SpawnFailed {
            os_error: Some(2),
            detail: "no such file".into()
        }),
        AzCliFacts::Unknown {
            detail: "no such file".into()
        }
    );
}

#[test]
fn running_a_program_that_is_not_there_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let got = az_cli_facts_at(&dir.path().join("no-az"), Duration::from_secs(2));
    assert!(matches!(got, AzCliFacts::Unknown { .. }), "{got:?}");
}

#[cfg(unix)]
#[test]
fn running_a_script_that_prints_the_az_banner_finds_the_version() {
    let dir = tempfile::tempdir().unwrap();
    let az = dir.path().join("az");
    puddle_test_kit::write_executable(
        &az,
        "#!/bin/sh\necho 'azure-cli                         2.64.0 *'\n",
    );
    assert_eq!(
        az_cli_facts_at(&az, Duration::from_secs(5)),
        AzCliFacts::Found {
            version: "2.64.0".into()
        }
    );
}

#[test]
fn no_az_in_any_path_folder_is_not_found() {
    let empty = tempfile::tempdir().unwrap();
    let path = std::env::join_paths([empty.path()]).unwrap();
    assert_eq!(
        az_cli_in(Some(path), Duration::from_secs(2)),
        AzCliFacts::NotFound
    );
    assert_eq!(
        az_cli_in(None, Duration::from_secs(2)),
        AzCliFacts::NotFound
    );
}

#[cfg(unix)]
#[test]
fn an_az_in_a_path_folder_is_found_and_asked_for_its_version() {
    let dir = tempfile::tempdir().unwrap();
    puddle_test_kit::write_executable(
        &dir.path().join("az"),
        "#!/bin/sh\necho 'azure-cli                         2.64.0 *'\n",
    );
    let path = std::env::join_paths([dir.path()]).unwrap();
    assert_eq!(
        az_cli_in(Some(path), Duration::from_secs(5)),
        AzCliFacts::Found {
            version: "2.64.0".into()
        }
    );
}
