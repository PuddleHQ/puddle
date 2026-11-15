// SPDX-License-Identifier: AGPL-3.0-or-later
//! `list_identities` and `sign` against a fake agent on a real owner-only pipe (Windows) or socket (Unix),
//! the same endpoint kinds a real agent uses.

#![expect(
    clippy::unwrap_used,
    reason = "test helpers outside #[test] fns fail the test by panicking"
)]

use std::time::{Duration, Instant};

use puddle_ipc::IpcRoot;
use puddle_ssh_agent::{AgentEndpoint, AgentError, list_identities, sign};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone, Copy)]
enum Behaviour {
    Keys,
    Failure,
    NoKeys,
    Signs,
    Garbage,
    Silent,
}

fn string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&u32::try_from(bytes.len()).unwrap().to_be_bytes());
    out.extend_from_slice(bytes);
}

fn key_blob(name: &str) -> Vec<u8> {
    let mut blob = Vec::new();
    string(&mut blob, name.as_bytes());
    blob.extend_from_slice(&[7; 8]);
    blob
}

fn framed(message: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(message.len()).unwrap().to_be_bytes().to_vec();
    out.extend_from_slice(message);
    out
}

fn answer(behaviour: Behaviour) -> Option<Vec<u8>> {
    match behaviour {
        Behaviour::Keys => {
            let mut m = vec![12];
            m.extend_from_slice(&2_u32.to_be_bytes());
            for (name, comment) in [("ssh-ed25519", "primary key"), ("ssh-rsa", "old key")] {
                string(&mut m, &key_blob(name));
                string(&mut m, comment.as_bytes());
            }
            Some(framed(&m))
        }
        Behaviour::NoKeys => Some(framed(&[12, 0, 0, 0, 0])),
        Behaviour::Failure => Some(framed(&[5])),
        Behaviour::Signs => {
            let mut m = vec![14];
            string(&mut m, b"SIGNATURE BLOB");
            Some(framed(&m))
        }
        Behaviour::Garbage => Some(framed(&[200, 1, 2])),
        Behaviour::Silent => None,
    }
}

/// A fake agent answering every client as `behaviour` says, on a fresh endpoint. The returned
/// guard keeps the endpoint and its runtime alive.
struct FakeAgent {
    endpoint: AgentEndpoint,
    requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    asked: std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>,
    _runtime: tokio::runtime::Runtime,
    _root: IpcRoot,
}

fn fake_agent(behaviour: Behaviour) -> FakeAgent {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let root = IpcRoot::new().unwrap();
    let mut listener = root.listen().unwrap();
    let endpoint = AgentEndpoint::at(listener.endpoint().path());
    let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = requests.clone();
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen = asked.clone();
    runtime.spawn(async move {
        // flake-ok: the accept loop of a fake server, not a wait for a condition.
        while let Ok(mut conn) = listener.accept().await {
            let counted = counted.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let mut len = [0_u8; 4];
                if conn.read_exact(&mut len).await.is_err() {
                    return;
                }
                let mut request = vec![0_u8; u32::from_be_bytes(len) as usize];
                if conn.read_exact(&mut request).await.is_err() {
                    return;
                }
                seen.lock().unwrap().push(request);
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                match answer(behaviour) {
                    Some(bytes) => {
                        let _sent = conn.write_all(&bytes).await;
                    }
                    None => tokio::time::sleep(Duration::from_secs(30)).await,
                }
            });
        }
    });
    FakeAgent {
        endpoint,
        requests,
        asked,
        _runtime: runtime,
        _root: root,
    }
}

const LIMIT: Duration = Duration::from_secs(5);

#[test]
fn it_lists_the_keys_the_agent_holds() {
    let agent = fake_agent(Behaviour::Keys);
    let keys = list_identities(&agent.endpoint, LIMIT).unwrap();
    let types: Vec<_> = keys.iter().map(|k| k.key_type().unwrap()).collect();
    assert_eq!(types, ["ssh-ed25519", "ssh-rsa"]);
    assert_eq!(keys[0].comment(), "primary key");
    assert_eq!(agent.requests.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn an_agent_with_no_keys_gives_an_empty_list() {
    let agent = fake_agent(Behaviour::NoKeys);
    assert_eq!(list_identities(&agent.endpoint, LIMIT).unwrap().len(), 0);
}

#[test]
fn a_locked_agent_is_a_refusal() {
    let agent = fake_agent(Behaviour::Failure);
    assert!(matches!(
        list_identities(&agent.endpoint, LIMIT),
        Err(AgentError::Refused)
    ));
}

#[test]
fn something_that_is_not_an_agent_is_a_protocol_error() {
    let agent = fake_agent(Behaviour::Garbage);
    assert!(matches!(
        list_identities(&agent.endpoint, LIMIT),
        Err(AgentError::Protocol(_))
    ));
}

#[test]
fn nothing_listening_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(windows)]
    let endpoint = AgentEndpoint::at(r"\\.\pipe\puddle-no-such-agent-pipe");
    #[cfg(not(windows))]
    let endpoint = AgentEndpoint::at(dir.path().join("no-agent.sock"));
    assert!(matches!(
        list_identities(&endpoint, LIMIT),
        Err(AgentError::Unreachable(_))
    ));
    drop(dir);
}

#[test]
fn an_agent_that_never_answers_ends_at_the_limit() {
    let agent = fake_agent(Behaviour::Silent);
    let limit = Duration::from_millis(300);
    let started = Instant::now();
    let result = list_identities(&agent.endpoint, limit);
    let took = started.elapsed();
    assert!(
        matches!(result, Err(AgentError::TimedOut(l)) if l == limit),
        "{result:?}"
    );
    assert!(took >= limit, "it waited the whole limit: {took:?}");
}

#[test]
fn the_key_list_request_is_the_one_message_sent_for_a_list() {
    let agent = fake_agent(Behaviour::Keys);
    list_identities(&agent.endpoint, LIMIT).unwrap();
    assert_eq!(*agent.asked.lock().unwrap(), vec![vec![11]]);
}

#[test]
fn a_sign_request_names_the_key_the_data_and_the_flags_and_gives_the_signature() {
    let agent = fake_agent(Behaviour::Signs);
    let signature = sign(&agent.endpoint, b"KEY", b"DATA", 4, LIMIT).unwrap();
    assert_eq!(signature, b"SIGNATURE BLOB");
    let mut want = vec![13];
    string(&mut want, b"KEY");
    string(&mut want, b"DATA");
    want.extend_from_slice(&4_u32.to_be_bytes());
    assert_eq!(*agent.asked.lock().unwrap(), vec![want]);
}

#[test]
fn a_sign_request_an_agent_refuses_or_garbles_or_never_answers_says_so() {
    let refused = fake_agent(Behaviour::Failure);
    assert!(matches!(
        sign(&refused.endpoint, b"K", b"D", 0, LIMIT),
        Err(AgentError::Refused)
    ));
    let garbled = fake_agent(Behaviour::Garbage);
    assert!(matches!(
        sign(&garbled.endpoint, b"K", b"D", 0, LIMIT),
        Err(AgentError::Protocol("not a sign response"))
    ));
    let silent = fake_agent(Behaviour::Silent);
    let limit = Duration::from_millis(200);
    assert!(matches!(
        sign(&silent.endpoint, b"K", b"D", 0, limit),
        Err(AgentError::TimedOut(l)) if l == limit
    ));
}
