// SPDX-License-Identifier: AGPL-3.0-or-later
use std::collections::BTreeSet;

use puddle_ssh_agent::request;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::*;

fn row(host: &str, keys: Option<&[&str]>) -> SshAgentHost {
    SshAgentHost {
        host: host.to_owned(),
        keys: keys.map(|k| k.iter().map(|s| (*s).to_owned()).collect()),
        changed_at: 0,
    }
}

#[test]
fn stored_hosts_become_the_proxy_hosts_with_every_key_or_the_listed_ones() {
    let hosts = allowed_hosts(vec![
        row("build.example.com", None),
        row("10.0.0.7", Some(&["SHA256:a", "SHA256:b"])),
        row("Not A Host!", None),
    ]);
    assert_eq!(hosts.len(), 2, "the row that does not parse is left out");
    assert_eq!(
        hosts[&Host::parse_normalised("build.example.com").unwrap()],
        AllowedKeys::All
    );
    assert_eq!(
        hosts[&Host::parse_normalised("10.0.0.7").unwrap()],
        AllowedKeys::Only(BTreeSet::from([
            "SHA256:a".to_owned(),
            "SHA256:b".to_owned()
        ]))
    );
}

/// An agent on a real socket (a named pipe on Windows) that holds one key, named by its comment
/// in what it signs so the test can tell two agents apart.
fn agent(name: &'static str) -> (String, puddle_ipc::IpcRoot) {
    let root = puddle_ipc::IpcRoot::new().unwrap();
    let mut listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_string_lossy().into_owned();
    tokio::spawn(async move {
        // flake-ok: the accept loop of a fake server, not a wait for a condition.
        while let Ok(mut conn) = listener.accept().await {
            tokio::spawn(async move {
                let mut len = [0_u8; 4];
                conn.read_exact(&mut len).await.unwrap();
                let mut asked = vec![0_u8; u32::from_be_bytes(len) as usize];
                conn.read_exact(&mut asked).await.unwrap();
                let answer = match asked.first() {
                    Some(11) => {
                        request::identities(&[&Identity::new(vec![1, 2, 3], String::new())])
                    }
                    _ => request::signature(name.as_bytes()),
                };
                conn.write_all(&answer).await.unwrap();
            });
        }
    });
    (path, root)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_agent_the_settings_name_is_asked_and_a_changed_setting_applies_to_the_next_request() {
    let dir = tempfile::tempdir().unwrap();
    let settings = Arc::new(crate::FileSettings::new(dir.path()));
    let (first, _a) = agent("first");
    let (second, _b) = agent("second");
    settings
        .save_global(serde_json::json!({"ui": {"ssh_agent_pipe": first}}))
        .unwrap();
    let host_agent = SettingsAgent {
        settings: Some(settings.clone() as Arc<dyn SettingsRepo>),
        auth_sock: None,
    };
    let host_agent = Arc::new(host_agent);
    let asked = Arc::clone(&host_agent);
    let (keys, signed) = tokio::task::spawn_blocking(move || {
        (asked.list().unwrap(), asked.sign(&[1], b"d", 0).unwrap())
    })
    .await
    .unwrap();
    assert_eq!(keys, vec![Identity::new(vec![1, 2, 3], String::new())]);
    assert_eq!(signed, b"first");

    settings
        .save_global(serde_json::json!({"ui": {"ssh_agent_pipe": second}}))
        .unwrap();
    let signed = tokio::task::spawn_blocking(move || host_agent.sign(&[1], b"d", 0).unwrap())
        .await
        .unwrap();
    assert_eq!(signed, b"second");
}

#[cfg(not(windows))]
#[test]
fn with_no_setting_the_auth_sock_is_asked_and_with_neither_there_is_no_agent() {
    let none = SettingsAgent {
        settings: None,
        auth_sock: None,
    };
    let err = none.list().unwrap_err();
    assert!(
        err.to_string().contains("SSH_AUTH_SOCK is not set"),
        "{err}"
    );
    assert!(none.sign(&[1], b"d", 0).is_err());

    let gone = SettingsAgent {
        settings: None,
        auth_sock: Some("/nonexistent/puddle-test-agent.sock".into()),
    };
    assert!(matches!(gone.list(), Err(AgentError::Unreachable(_))));
}

#[test]
fn a_new_agent_reads_the_auth_sock_of_its_process() {
    let made = SettingsAgent::new(None);
    assert_eq!(made.auth_sock, std::env::var_os("SSH_AUTH_SOCK"));
}
