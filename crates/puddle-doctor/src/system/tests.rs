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

fn data_with_store(store: puddle_secrets::StoreKind) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    puddle_secrets::write_record(dir.path(), store, puddle_secrets::Rule::Headless).unwrap();
    dir
}

/// A probe and a session that fail the test when asked: a file vault asks neither.
fn never<T>() -> T {
    panic!("a file vault asks neither the keyring nor loginctl");
}

#[test]
fn a_data_folder_that_records_the_file_vault_reads_its_header_and_asks_nothing_else() {
    use puddle_secrets::{FileVault, KeySourceTag, StoreKind, VaultKey, vault_path};
    let dir = data_with_store(StoreKind::File);
    // The record names the vault and there is none yet.
    assert_eq!(
        credential_store_facts(Some(dir.path()), never, never),
        CredentialStoreFacts::File(VaultFacts::NoVaultYet)
    );
    FileVault::create(
        &vault_path(dir.path()),
        VaultKey::from_bytes([7; 32]),
        KeySourceTag::SystemdHostTpm2,
    )
    .unwrap();
    assert_eq!(
        credential_store_facts(Some(dir.path()), never, never),
        CredentialStoreFacts::File(VaultFacts::Header(KeySourceTag::SystemdHostTpm2))
    );
    std::fs::write(vault_path(dir.path()), b"not a vault at all").unwrap();
    assert_eq!(
        credential_store_facts(Some(dir.path()), never, never),
        CredentialStoreFacts::File(VaultFacts::Unreadable)
    );
}

#[test]
fn the_operating_system_store_is_probed_when_it_is_recorded_or_nothing_is_decided() {
    use puddle_secrets::{LoginSession, StoreHealth, StoreKind};
    let recorded = data_with_store(StoreKind::Os);
    assert_eq!(
        credential_store_facts(
            Some(recorded.path()),
            || StoreHealth::Locked,
            || LoginSession::NotGraphical
        ),
        CredentialStoreFacts::Os {
            recorded: true,
            health: StoreHealth::Locked,
            session: LoginSession::NotGraphical
        }
    );
    let fresh = tempfile::tempdir().unwrap();
    for data in [Some(fresh.path()), None] {
        assert_eq!(
            credential_store_facts(data, || StoreHealth::Working, || LoginSession::Graphical),
            CredentialStoreFacts::Os {
                recorded: false,
                health: StoreHealth::Working,
                session: LoginSession::Graphical
            }
        );
    }
}

#[test]
fn a_record_that_cannot_be_read_is_reported_and_nothing_is_probed() {
    let dir = tempfile::tempdir().unwrap();
    let folder = puddle_secrets::secrets_folder(dir.path());
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("store.json"), "{").unwrap();
    assert_eq!(
        credential_store_facts(Some(dir.path()), never, never),
        CredentialStoreFacts::Unreadable
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_login_session_is_read_from_loginctl_found_on_the_path() {
    use puddle_secrets::LoginSession;
    // A new folder for each script: a file that was just written is not run again in place.
    let answer = |display: &str, kind: &str| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("loginctl");
        let body = format!(
            "#!/bin/sh\ncase \"$1\" in show-user) echo '{display}';; show-session) echo '{kind}';; esac\n"
        );
        puddle_test_kit::write_executable(&path, &body);
        login_session_in(
            Some(dir.path().as_os_str().to_owned()),
            Duration::from_secs(5),
        )
    };
    assert_eq!(answer("c3", "wayland"), LoginSession::Graphical);
    assert_eq!(answer("c1", "tty"), LoginSession::NotGraphical);
    assert_eq!(answer("", ""), LoginSession::NotGraphical);
    assert_eq!(answer("--help", ""), LoginSession::Unknown);
    // A display session whose type cannot be read, or a user whose display cannot be read, is not guessed at.
    for body in [
        "#!/bin/sh\ncase \"$1\" in show-user) echo c1;; *) exit 1;; esac\n",
        "#!/bin/sh\nexit 1\n",
    ] {
        let dir = tempfile::tempdir().unwrap();
        puddle_test_kit::write_executable(&dir.path().join("loginctl"), body);
        assert_eq!(
            login_session_in(
                Some(dir.path().as_os_str().to_owned()),
                Duration::from_secs(5)
            ),
            LoginSession::Unknown
        );
    }
    assert_eq!(
        login_session_in(Some("/nonexistent".into()), Duration::from_secs(1)),
        LoginSession::Unknown
    );
}
