// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_ca::CaBuilder;
use puddle_ssh_agent::filter::Refusal;
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

/// The gate in memory: its answer to the first-use question, what it was asked and what it
/// recorded.
#[derive(Default)]
struct FakeGate {
    answer: Mutex<Option<Disapproval>>,
    asked: Mutex<Vec<(WorkspaceId, String, Vec<u8>)>>,
    recorded: Mutex<Vec<(WorkspaceId, Option<String>, Outcome)>>,
    /// The host key kept per workspace and host.
    pinned: Mutex<HashMap<(WorkspaceId, String), String>>,
}

impl AgentGate for FakeGate {
    fn pin_host_key(
        &self,
        workspace: &WorkspaceId,
        host: &str,
        host_key: &str,
    ) -> Result<(), PinFailure> {
        let mut pinned = self.pinned.lock().unwrap();
        let kept = pinned
            .entry((workspace.clone(), host.to_owned()))
            .or_insert_with(|| host_key.to_owned());
        if kept == host_key {
            Ok(())
        } else {
            Err(PinFailure::Changed(kept.clone()))
        }
    }

    fn approve(&self, workspace: &WorkspaceId, host: &str, key: &[u8]) -> Result<(), Disapproval> {
        self.asked
            .lock()
            .unwrap()
            .push((workspace.clone(), host.to_owned(), key.to_vec()));
        self.answer.lock().unwrap().map_or(Ok(()), Err)
    }

    fn record(&self, workspace: &WorkspaceId, host: Option<&str>, outcome: &Outcome) {
        self.recorded.lock().unwrap().push((
            workspace.clone(),
            host.map(str::to_owned),
            outcome.clone(),
        ));
    }
}

struct World {
    server: TestKey,
    user: TestKey,
    agent: Arc<FakeAgent>,
    gate: Arc<FakeGate>,
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
    let gate = Arc::new(FakeGate::default());
    let agents = Arc::new(SshAgents::new(agent.clone(), gate.clone()));
    let w = World {
        server: TestKey::ed25519(1),
        user,
        agent,
        gate,
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
            Some(self.terminations.clone()),
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
async fn the_first_use_question_names_the_workspace_host_and_key_and_every_outcome_is_recorded() {
    let w = world();
    let _tunnel = w.spliced("box", "build.example");
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    ask(&mut guest, &w.the_bind()).await;
    ask(&mut guest, &framed(&[11])).await;
    let data = login(&SESSION, &w.user.blob(), Some(&w.server.blob()));
    ask(&mut guest, &framed(&sign_request(&w.user.blob(), &data, 0))).await;
    assert_eq!(
        *w.gate.asked.lock().unwrap(),
        vec![(workspace("box"), "build.example".to_owned(), w.user.blob())]
    );
    let outcomes: Vec<_> = w
        .gate
        .recorded
        .lock()
        .unwrap()
        .iter()
        .map(|(ws, host, outcome)| (ws.to_string(), host.clone(), outcome.clone()))
        .collect();
    let host = Some("build.example".to_owned());
    assert_eq!(
        outcomes,
        vec![
            (
                "box".to_owned(),
                host.clone(),
                Outcome::Bound("build.example".into())
            ),
            ("box".to_owned(), host.clone(), Outcome::Listed(1)),
            (
                "box".to_owned(),
                host,
                Outcome::Signed {
                    family: "ed25519",
                    fingerprint: w.user.fingerprint()
                }
            ),
        ]
    );
}

#[tokio::test]
async fn a_first_use_the_user_does_not_approve_is_refused_and_nothing_is_signed() {
    let w = world();
    *w.gate.answer.lock().unwrap() = Some(Disapproval::Denied);
    let _tunnel = w.spliced("box", "build.example");
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    ask(&mut guest, &w.the_bind()).await;
    let data = login(&SESSION, &w.user.blob(), Some(&w.server.blob()));
    assert_eq!(
        ask(&mut guest, &framed(&sign_request(&w.user.blob(), &data, 0))).await,
        request::failure()
    );
    assert!(w.agent.signed.lock().unwrap().is_empty());
    let recorded = w.gate.recorded.lock().unwrap();
    assert_eq!(
        recorded.last().map(|r| r.2.clone()),
        Some(Outcome::Refused(Refusal::Disapproved(Disapproval::Denied)))
    );
}

#[tokio::test]
async fn a_server_at_the_name_that_shows_another_host_key_gets_no_login_signed() {
    let w = world();
    let first = w.spliced("box", "build.example");
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    assert_eq!(ask(&mut guest, &w.the_bind()).await, request::success());
    drop(first);
    // The server at build.example now relays another server's key exchange.
    let elsewhere = TestKey::ed25519(9);
    let mut relay = KexWatch::new(
        Arc::clone(&w.agents),
        workspace("box"),
        host("build.example"),
    );
    relay.feed(&server_kex(&elsewhere, &elsewhere.sign(&SESSION)));
    let (mut guest, _served) = w.stream("box", IDLE_LIMIT);
    let relayed = framed(&bind(
        &elsewhere.blob(),
        &SESSION,
        &elsewhere.sign(&SESSION),
        false,
    ));
    assert_eq!(ask(&mut guest, &relayed).await, request::failure());
    let data = login(&SESSION, &w.user.blob(), Some(&elsewhere.blob()));
    assert_eq!(
        ask(&mut guest, &framed(&sign_request(&w.user.blob(), &data, 0))).await,
        request::failure()
    );
    assert!(w.agent.signed.lock().unwrap().is_empty());
    let recorded = w.gate.recorded.lock().unwrap();
    assert!(recorded.iter().any(|(_, _, outcome)| *outcome
        == Outcome::Refused(Refusal::HostKeyChanged {
            kept: w.server.fingerprint(),
            shown: elsewhere.fingerprint(),
        })));
    // The first host key stays the one kept.
    assert_eq!(
        w.gate
            .pinned
            .lock()
            .unwrap()
            .get(&(workspace("box"), "build.example".to_owned())),
        Some(&w.server.fingerprint())
    );
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
    w.agents = Arc::new(SshAgents::new(Arc::new(PanickingAgent), w.gate.clone()));
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

/// A one-way latch: threads wait on it (at most 10 s) until it is opened.
#[derive(Default)]
struct Latch(Mutex<bool>, std::sync::Condvar);

impl Latch {
    fn open(&self) {
        *self.0.lock().unwrap() = true;
        self.1.notify_all();
    }

    fn wait(&self) {
        let open = self.0.lock().unwrap();
        drop(
            self.1
                .wait_timeout_while(open, Duration::from_secs(10), |open| !*open)
                .unwrap(),
        );
    }
}

/// A gate whose `record` (which runs on the blocking thread of every request) holds the thread
/// until released, counting the threads held at once per workspace.
#[derive(Default)]
struct HoldingGate {
    active: Mutex<HashMap<WorkspaceId, usize>>,
    peak: Mutex<HashMap<WorkspaceId, usize>>,
    release: Latch,
}

impl AgentGate for HoldingGate {
    fn pin_host_key(&self, _: &WorkspaceId, _: &str, _: &str) -> Result<(), PinFailure> {
        Ok(())
    }

    fn approve(&self, _: &WorkspaceId, _: &str, _: &[u8]) -> Result<(), Disapproval> {
        Ok(())
    }

    fn record(&self, workspace: &WorkspaceId, _: Option<&str>, _: &Outcome) {
        let now = {
            let mut active = self.active.lock().unwrap();
            let n = active.entry(workspace.clone()).or_default();
            *n += 1;
            *n
        };
        {
            let mut peak = self.peak.lock().unwrap();
            let p = peak.entry(workspace.clone()).or_default();
            *p = (*p).max(now);
        }
        self.release.wait();
        *self.active.lock().unwrap().get_mut(workspace).unwrap() -= 1;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_workspace_holds_at_most_its_cap_of_blocking_threads_however_many_streams_wait() {
    let gate = Arc::new(HoldingGate::default());
    let agents = Arc::new(SshAgents::new(Arc::new(FakeAgent::default()), gate.clone()));
    let cap = MAX_BLOCKED_PER_WORKSPACE;
    let streams = cap * 3;
    let mut open = Vec::new();
    for _ in 0..streams {
        let (mut guest, host_end) = duplex(64 * 1024);
        let served = tokio::spawn(serve(
            Arc::clone(&agents),
            None,
            workspace("box"),
            host_end,
            IDLE_LIMIT,
        ));
        guest
            .write_all(&puddle_ssh_agent::wire::request_identities())
            .await
            .unwrap();
        open.push((guest, served));
    }
    puddle_test_kit::eventually("the cap of requests hold a thread", WAIT, || {
        gate.active.lock().unwrap().get(&workspace("box")).copied() == Some(cap)
    })
    .await;
    // Absence window: no signal says another request did not start, so give it time to.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        gate.active.lock().unwrap().get(&workspace("box")).copied(),
        Some(cap),
        "the rest wait without a thread"
    );
    gate.release.open();
    for (mut guest, served) in open {
        let mut len = [0_u8; 4];
        tokio::time::timeout(WAIT, guest.read_exact(&mut len))
            .await
            .unwrap()
            .unwrap();
        drop(guest);
        tokio::time::timeout(WAIT, served).await.unwrap().unwrap();
    }
    assert_eq!(gate.peak.lock().unwrap()[&workspace("box")], cap);
}
