// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_ca::CaBuilder;
use puddle_ssh_agent::request;
use puddle_ssh_agent::testing::{TestKey, bind, framed, login, server_kex, sign_request};
use tokio::io::{DuplexStream, duplex};

use super::*;
use crate::terminate::{NoInjection, Termination, TerminationSet, Terminations};

const SESSION: [u8; 32] = [0x5e; 32];
const WAIT: Duration = Duration::from_secs(5);

fn workspace(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

fn host(h: &str) -> Host {
    Host::parse_normalised(h).unwrap()
}

/// The user's agent, in memory: it holds `keys` and signs anything with `SIGNATURE`.
#[derive(Default)]
struct FakeAgent {
    keys: Vec<Identity>,
    signed: Mutex<Vec<Vec<u8>>>,
}

impl HostAgent for FakeAgent {
    fn list(&self) -> Result<Vec<Identity>, AgentError> {
        Ok(self.keys.clone())
    }

    fn sign(&self, _key: &[u8], data: &[u8], _flags: u32) -> Result<Vec<u8>, AgentError> {
        self.signed.lock().unwrap().push(data.to_vec());
        Ok(b"SIGNATURE".to_vec())
    }
}

struct World {
    server: TestKey,
    user: TestKey,
    agent: Arc<FakeAgent>,
    agents: Arc<SshAgents>,
    terminations: Arc<Terminations>,
}

/// The user's agent holds one key; workspace `box` has the agent on for `build.example` with
/// every key.
fn world() -> World {
    let user = TestKey::ed25519(2);
    let agent = Arc::new(FakeAgent {
        keys: vec![Identity::new(user.blob(), "me@desk".into())],
        ..FakeAgent::default()
    });
    let agents = Arc::new(SshAgents::new(agent.clone()));
    let w = World {
        server: TestKey::ed25519(1),
        user,
        agent,
        agents,
        terminations: Arc::new(Terminations::new()),
    };
    w.switch(&[("build.example", AllowedKeys::All)]);
    w
}

impl World {
    /// Sets the hosts workspace `box` has the agent on for.
    fn switch(&self, hosts: &[(&str, AllowedKeys)]) {
        let ca = Arc::new(CaBuilder::new("test").build().unwrap());
        let hosts = hosts
            .iter()
            .map(|(h, keys)| (host(h), keys.clone()))
            .collect();
        self.terminations.insert(
            workspace("box"),
            Termination::new(TerminationSet::new(), ca, Arc::new(NoInjection))
                .with_ssh_agent_hosts(hosts),
        );
    }

    /// A spliced connection of `ws` to `to` whose server sent its key exchange.
    fn spliced(&self, ws: &str, to: &str) -> KexWatch {
        let mut watch = KexWatch::new(Arc::clone(&self.agents), workspace(ws), host(to));
        watch.feed(&server_kex(&self.server, &self.server.sign(&SESSION)));
        watch
    }

    fn the_bind(&self) -> Vec<u8> {
        framed(&bind(
            &self.server.blob(),
            &SESSION,
            &self.server.sign(&SESSION),
            false,
        ))
    }

    /// An agent stream of `ws` served with `idle` as the limit; the guest's end comes back.
    fn stream(&self, ws: &str, idle: Duration) -> (DuplexStream, tokio::task::JoinHandle<()>) {
        let (guest, host_end) = duplex(64 * 1024);
        let served = tokio::spawn(serve(
            Arc::clone(&self.agents),
            Some(Arc::clone(&self.terminations) as Arc<dyn TerminationSource>),
            workspace(ws),
            host_end,
            idle,
        ));
        (guest, served)
    }
}

/// Sends one framed request and reads one framed answer.
async fn ask(guest: &mut DuplexStream, message: &[u8]) -> Vec<u8> {
    guest.write_all(message).await.unwrap();
    let mut len = [0_u8; 4];
    tokio::time::timeout(WAIT, guest.read_exact(&mut len))
        .await
        .unwrap()
        .unwrap();
    let mut body = vec![0_u8; u32::from_be_bytes(len) as usize];
    guest.read_exact(&mut body).await.unwrap();
    framed(&body)
}

async fn closed(guest: &mut DuplexStream, served: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(WAIT, served).await.unwrap().unwrap();
    let mut rest = Vec::new();
    guest.read_to_end(&mut rest).await.unwrap();
    assert_eq!(rest, b"");
}

fn listed(keys: &[&TestKey]) -> Vec<u8> {
    let ids: Vec<Identity> = keys
        .iter()
        .map(|k| Identity::new(k.blob(), String::new()))
        .collect();
    request::identities(&ids.iter().collect::<Vec<_>>())
}

#[test]
fn a_seen_session_is_kept_for_its_workspace_while_the_tunnel_lives() {
    let w = world();
    let watch = w.spliced("box", "build.example");
    let key = w.server.key_data();
    let signature = w.server.sign(&SESSION);
    assert_eq!(
        w.agents.find(&workspace("box"), &key, &signature),
        Some(host("build.example"))
    );
    assert_eq!(w.agents.find(&workspace("other"), &key, &signature), None);
    assert_eq!(w.agents.find(&workspace("box"), &key, b"another"), None);
    drop(watch);
    assert_eq!(w.agents.find(&workspace("box"), &key, &signature), None);
    assert_eq!(w.agents.sessions().len(), 0);
}

#[test]
fn a_key_exchange_in_small_writes_is_seen_once_and_a_silent_tunnel_keeps_nothing() {
    let w = world();
    let mut watch = KexWatch::new(
        Arc::clone(&w.agents),
        workspace("box"),
        host("build.example"),
    );
    let bytes = server_kex(&w.server, &w.server.sign(&SESSION));
    for chunk in bytes.chunks(7) {
        watch.feed(chunk);
    }
    // What follows the key exchange (encrypted) adds nothing.
    watch.feed(&[0xaa; 256]);
    assert_eq!(w.agents.sessions().len(), 1);
    let silent = KexWatch::new(Arc::clone(&w.agents), workspace("box"), host("x.example"));
    drop(silent);
    drop(watch);
    assert_eq!(w.agents.sessions().len(), 0);
}

#[tokio::test]
async fn a_stream_binds_to_a_seen_session_lists_and_signs_its_login() {
    let w = world();
    let _tunnel = w.spliced("box", "build.example");
    let (mut guest, served) = w.stream("box", IDLE_LIMIT);
    assert_eq!(ask(&mut guest, &w.the_bind()).await, request::success());
    assert_eq!(ask(&mut guest, &framed(&[11])).await, listed(&[&w.user]));
    let data = login(&SESSION, &w.user.blob(), Some(&w.server.blob()));
    assert_eq!(
        ask(&mut guest, &framed(&sign_request(&w.user.blob(), &data, 0))).await,
        request::signature(b"SIGNATURE")
    );
    assert_eq!(*w.agent.signed.lock().unwrap(), vec![data]);
    guest.shutdown().await.unwrap();
    closed(&mut guest, served).await;
}

#[tokio::test]
async fn a_bind_is_refused_for_another_workspace_an_unseen_session_or_a_host_switched_off() {
    let w = world();
    // Seen, but on another workspace's tunnel.
    let other = w.spliced("other", "build.example");
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    assert_eq!(ask(&mut guest, &w.the_bind()).await, request::failure());
    drop(other);
    // Seen on this workspace, to a host the agent is not on for.
    let _tunnel = w.spliced("box", "lab.example");
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    assert_eq!(ask(&mut guest, &w.the_bind()).await, request::failure());
    // No list before a bind: an empty one.
    assert_eq!(ask(&mut guest, &framed(&[11])).await, listed(&[]));
    assert!(w.agent.signed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_workspace_without_a_termination_has_the_agent_off_everywhere() {
    let w = world();
    let _tunnel = w.spliced("box", "build.example");
    let (guest, host_end) = duplex(4096);
    let served = tokio::spawn(serve(
        Arc::clone(&w.agents),
        None,
        workspace("box"),
        host_end,
        IDLE_LIMIT,
    ));
    let mut guest = guest;
    assert_eq!(ask(&mut guest, &w.the_bind()).await, request::failure());
    guest.shutdown().await.unwrap();
    closed(&mut guest, served).await;
}

#[tokio::test]
async fn turning_the_switch_off_refuses_the_next_request_of_a_bound_stream() {
    let w = world();
    let _tunnel = w.spliced("box", "build.example");
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    assert_eq!(ask(&mut guest, &w.the_bind()).await, request::success());
    w.switch(&[]);
    assert_eq!(ask(&mut guest, &framed(&[11])).await, listed(&[]));
    let data = login(&SESSION, &w.user.blob(), None);
    assert_eq!(
        ask(&mut guest, &framed(&sign_request(&w.user.blob(), &data, 0))).await,
        request::failure()
    );
    assert!(w.agent.signed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_message_of_the_largest_length_is_answered() {
    let w = world();
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    let mut message = vec![99_u8];
    message.resize(MAX_MESSAGE, 0);
    assert_eq!(ask(&mut guest, &framed(&message)).await, request::failure());
}

#[test]
fn the_registry_and_a_watch_name_themselves_in_debug_output() {
    let w = world();
    assert!(format!("{:?}", w.agents).starts_with("SshAgents"));
    let watch = w.spliced("box", "build.example");
    assert!(format!("{watch:?}").contains("build.example"));
}

/// A user's agent on a real socket (named pipe on Windows) that holds one key and signs anything.
fn agent_on_an_endpoint(key: Vec<u8>) -> (AgentAt, puddle_ipc::IpcRoot) {
    let root = puddle_ipc::IpcRoot::new().unwrap();
    let mut listener = root.listen().unwrap();
    let endpoint = AgentEndpoint::at(listener.endpoint().path());
    tokio::spawn(async move {
        // flake-ok: the accept loop of a fake server, not a wait for a condition.
        while let Ok(mut conn) = listener.accept().await {
            let key = key.clone();
            tokio::spawn(async move {
                let mut len = [0_u8; 4];
                conn.read_exact(&mut len).await.unwrap();
                let mut asked = vec![0_u8; u32::from_be_bytes(len) as usize];
                conn.read_exact(&mut asked).await.unwrap();
                let answer = match asked.first() {
                    Some(11) => request::identities(&[&Identity::new(key, "c".into())]),
                    _ => request::signature(b"SIGNED BY THE AGENT"),
                };
                conn.write_all(&answer).await.unwrap();
            });
        }
    });
    (AgentAt::new(endpoint), root)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_agent_at_an_endpoint_lists_and_signs_through_it() {
    let user = TestKey::ed25519(2);
    let (agent, _root) = agent_on_an_endpoint(user.blob());
    let (keys, signature) = tokio::task::spawn_blocking(move || {
        (agent.list().unwrap(), agent.sign(&[1], b"data", 0).unwrap())
    })
    .await
    .unwrap();
    // The fake answers with `request::identities`, which blanks comments.
    assert_eq!(keys, vec![Identity::new(user.blob(), String::new())]);
    assert_eq!(signature, b"SIGNED BY THE AGENT");
}

/// An agent whose client panics (a bug there must close one stream, not take the proxy down).
struct PanickingAgent;

impl HostAgent for PanickingAgent {
    fn list(&self) -> Result<Vec<Identity>, AgentError> {
        panic!("the agent client broke");
    }

    fn sign(&self, _key: &[u8], _data: &[u8], _flags: u32) -> Result<Vec<u8>, AgentError> {
        panic!("the agent client broke");
    }
}

#[tokio::test]
async fn a_panic_while_answering_closes_only_that_stream() {
    let mut w = world();
    w.agents = Arc::new(SshAgents::new(Arc::new(PanickingAgent)));
    let _tunnel = w.spliced("box", "build.example");
    let (mut guest, served) = w.stream("box", IDLE_LIMIT);
    assert_eq!(ask(&mut guest, &w.the_bind()).await, request::success());
    guest.write_all(&framed(&[11])).await.unwrap();
    closed(&mut guest, served).await;
}

#[tokio::test]
async fn a_guest_gone_before_its_answer_ends_the_stream() {
    let w = world();
    let (mut guest, served) = w.stream("box", IDLE_LIMIT);
    // The request is in the pipe; the guest's end is gone before the answer can be written.
    guest.write_all(&framed(&[11])).await.unwrap();
    drop(guest);
    tokio::time::timeout(WAIT, served).await.unwrap().unwrap();
}

#[tokio::test]
async fn a_bad_length_closes_the_stream() {
    let w = world();
    for length in [0_u32, u32::try_from(MAX_MESSAGE).unwrap() + 1] {
        let (mut guest, served) = w.stream("box", IDLE_LIMIT);
        guest.write_all(&length.to_be_bytes()).await.unwrap();
        closed(&mut guest, served).await;
    }
    // A message cut short by the guest closing.
    let (mut guest, served) = w.stream("box", IDLE_LIMIT);
    guest.write_all(&[0, 0, 0, 9, 11]).await.unwrap();
    guest.shutdown().await.unwrap();
    closed(&mut guest, served).await;
}

#[tokio::test(start_paused = true)]
async fn a_silent_stream_is_closed_after_the_idle_limit() {
    let w = world();
    // Paused time runs ahead while every task waits: the idle limit (1 s) ends the stream before
    // the test's own wait (5 s) does.
    let (mut guest, served) = w.stream("box", Duration::from_secs(1));
    closed(&mut guest, served).await;
}
