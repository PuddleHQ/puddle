// SPDX-License-Identifier: AGPL-3.0-or-later
//! The filtered SSH agent end to end on a real microVM: the guest's own OpenSSH logs in to an SSH
//! server by a key that only the user's agent on the host holds. The guest has an agent socket,
//! `SSH_AUTH_SOCK` and `IdentityAgent`, but no key: each request goes to Puddle's filter, which
//! lists keys only after ssh bound the connection to the session it saw in clear, and signs only
//! that session's login.
//!
//! The SSH server is russh, reached by name only through a company proxy (`NameProxy`), so what
//! reaches it is the spliced connection Puddle carried. The user's agent is a small one in the test
//! on an owner-only endpoint (a socket, or a named pipe on Windows) that `ui.ssh_agent_pipe` names.
#![expect(
    clippy::unwrap_used,
    reason = "VM test: a failed step fails the test; measurements go to stderr"
)]

mod support;
mod vm_support;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_e2e::lab::NameProxy;
use puddle_ssh_agent::Identity;
use puddle_ssh_agent::request::{self, Request};
use puddle_ssh_agent::testing::TestKey;
use puddle_types::WorkspaceId;
use russh::server::{
    Auth, ChannelOpenHandle, Config, Handler, Msg, PublicKeyOffer, Session, VerifiedPublicKey,
};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use serde_json::json;
use support::{Api, ended};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use vm_support::{
    HarnessPlatform, create_workspace_from, guest, guest_as, start_vm_host_platform,
    through_company_proxy, with_fake_names,
};

const NAME: &str = "build.invalid";
/// The jump host of the hostile-jump test: the only host the agent is on for.
const JUMP: &str = "jump.invalid";
/// A name of the host the jump test must not log in to, as a GitHub address literal.
const GITHUB_LITERAL: &str = "140.82.112.3";
/// The same host under a name the guest's own ssh config gives it.
const GITHUB_NAME: &str = "github.invalid";
const SOCKET: &str = "/run/puddle/ssh-agent.sock";

/// What the test's agent was asked, in order: `list` or `sign`.
type Asked = Arc<Mutex<Vec<&'static str>>>;

/// The user's agent: one Ed25519 key, which it lists and signs with.
fn user_agent(key: Arc<TestKey>, asked: Asked) -> (puddle_ipc::IpcRoot, String) {
    let root = puddle_ipc::IpcRoot::new().unwrap();
    let mut listener = root.listen().unwrap();
    let path = listener.endpoint().path().to_string_lossy().into_owned();
    tokio::spawn(async move {
        while let Ok(mut conn) = listener.accept().await {
            let (key, asked) = (Arc::clone(&key), Arc::clone(&asked));
            tokio::spawn(async move {
                loop {
                    let Ok(len) = conn.read_u32().await else {
                        return;
                    };
                    let mut message = vec![0u8; usize::try_from(len).unwrap()];
                    if conn.read_exact(&mut message).await.is_err() {
                        return;
                    }
                    let answer = match request::parse(&message) {
                        Ok(Request::List) => {
                            asked
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push("list");
                            let me = Identity::new(key.blob(), "me@desk".into());
                            request::identities(&[&me])
                        }
                        Ok(Request::Sign {
                            key: wanted, data, ..
                        }) if wanted == key.blob() => {
                            asked
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .push("sign");
                            request::signature(&key.sign(&data))
                        }
                        _ => request::failure(),
                    };
                    if conn.write_all(&answer).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (root, path)
}

/// One SSH connection: a key login (any key whose signature checks), then each command answers
/// `logged in` and exits 0.
struct Server {
    logins: Arc<Mutex<Vec<String>>>,
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait's methods are async; the server answers at once"
)]
impl Handler for Server {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        self.logins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(
                key.public_key
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string(),
            );
        Ok(Auth::Accept)
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        session.data(channel, b"logged in\n".to_vec())?;
        session.exit_status_request(channel, 0)?;
        session.eof(channel)?;
        session.close(channel)
    }
}

/// The SSH server on a local port; the fingerprints of the keys that logged in go to `logins`.
async fn ssh_server(logins: Arc<Mutex<Vec<String>>>) -> std::net::SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = Arc::new(Config {
        keys: vec![russh::keys::PrivateKey::from(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[9; 32]),
        )],
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        inactivity_timeout: Some(Duration::from_secs(120)),
        auth_rejection_time: Duration::ZERO,
        ..Config::default()
    });
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let server = Server {
                logins: Arc::clone(&logins),
            };
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, stream, server).await {
                    let _ = session.await;
                }
            });
        }
    });
    addr
}

/// `ssh -v` as the session user in a login shell, as a terminal of the workspace runs it.
async fn login(vm: &vm_support::VmHost, workspace: &WorkspaceId, port: u16) -> (i32, String) {
    guest_as(
        &vm.runtime,
        workspace,
        "agent",
        &format!(
            "ssh -v -o BatchMode=yes -o StrictHostKeyChecking=accept-new \
             -o UserKnownHostsFile=/dev/null -o ConnectTimeout=30 -p {port} tester@{NAME} run 2>&1"
        ),
        Duration::from_secs(120),
    )
    .await
}

/// The guest's agent socket belongs to the session user and `SSH_AUTH_SOCK` and `IdentityAgent`
/// name it.
async fn socket_is_the_session_users(vm: &vm_support::VmHost, workspace: &WorkspaceId, port: u16) {
    let (code, setup) = guest_as(
        &vm.runtime,
        workspace,
        "agent",
        &format!(
            "for i in $(seq 50); do [ -S {SOCKET} ] && break; sleep 0.2; done; \
             echo \"owner=$(stat -c '%U %a' {SOCKET})\"; echo \"sock=$SSH_AUTH_SOCK\"; \
             ssh -G -p {port} {NAME} | grep -i '^identityagent'"
        ),
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(code, 0, "{setup}");
    assert!(setup.contains("owner=agent 600"), "{setup}");
    assert!(setup.contains(&format!("sock={SOCKET}")), "{setup}");
    assert!(
        setup.contains(&format!("identityagent {SOCKET}")),
        "{setup}"
    );
}

/// A question of the inbox, as `GET /api/ssh-agent-asks` lists it.
#[derive(serde::Deserialize)]
struct Ask {
    id: i64,
    workspace: String,
    host: String,
    key_type: String,
    fingerprint: String,
}

/// The first use asks the user: waits for the question in the inbox list, checks what it names
/// (this workspace, this host, the user's key) and gives `answer`. The guest's `ssh` waits for it.
async fn answer_first_use(api: &Api, workspace: &str, key: &TestKey, answer: &str) {
    answer_first_use_for(api, workspace, key, NAME, answer).await;
}

/// [`answer_first_use`] for the question about `host`.
async fn answer_first_use_for(api: &Api, workspace: &str, key: &TestKey, host: &str, answer: &str) {
    #[derive(serde::Deserialize)]
    struct Asks {
        asks: Vec<Ask>,
    }
    let ask = puddle_test_kit::eventually_async(
        "the first-use question reaches the inbox",
        Duration::from_secs(90),
        || async {
            let reply = api.get("/api/ssh-agent-asks").await;
            let list: Asks = serde_json::from_str(&reply.body).unwrap();
            list.asks.into_iter().next()
        },
    )
    .await;
    assert_eq!(ask.workspace, workspace);
    assert_eq!(ask.host, host);
    assert_eq!(ask.key_type, "ssh-ed25519");
    assert_eq!(ask.fingerprint, key.fingerprint());
    let reply = api
        .post(
            &format!(
                "/api/workspaces/{workspace}/ssh-agent-asks/{}/answer",
                ask.id
            ),
            &json!({ "answer": answer }).to_string(),
        )
        .await;
    assert_eq!(reply.status, 204, "{}", reply.body);
}

/// With the agent on for `build.invalid`, the guest's `ssh` logs in there with the user's key:
/// the socket belongs to the session user, `SSH_AUTH_SOCK` and `IdentityAgent` name it, ssh binds
/// the connection before it asks for the key list (an earlier list gets no key, so the login
/// proves the order too), and the host's agent is asked for the list, then one signature. Turned
/// off, the next login gets no key and the host's agent is not asked to sign.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_ssh_agent_logs_in_with_the_users_key_through_the_filter_and_stops_when_turned_off() {
    let key = Arc::new(TestKey::ed25519(4));
    let asked: Asked = Arc::default();
    let (_agent_root, agent_path) = user_agent(Arc::clone(&key), Arc::clone(&asked));
    let logins = Arc::new(Mutex::new(Vec::new()));
    let server = ssh_server(Arc::clone(&logins)).await;
    let proxy = NameProxy::start(&[(NAME, server)]).await;
    let options = with_fake_names(
        through_company_proxy(support::named_id_options(), &proxy),
        [NAME],
    );
    let vm = start_vm_host_platform("sshag", options, &HarnessPlatform::default(), |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    // The agent is named in the settings file only (the API does not take it), as a user does.
    vm.host
        .settings()
        .save_global(json!({"schema_version": 2, "ui": {"ssh_agent_pipe": agent_path}}))
        .unwrap();
    let workspace =
        create_workspace_from(&vm, &api, "sshag", &[NAME], json!({"memory_mib": 2048})).await;
    let id = workspace.as_str();

    // Off by default: no socket, no variable.
    let off = guest(&vm.runtime, &workspace, &format!("ls {SOCKET} 2>&1; true")).await;
    assert!(off.contains("No such file"), "{off}");

    // On for the host; the guest's socket comes with the next start.
    let on = api
        .put(
            &format!("/api/workspaces/{id}/ssh-agent/{NAME}"),
            &json!({"keys": null}).to_string(),
        )
        .await;
    assert_eq!(on.status, 200, "{}", on.body);
    let mut events = api.events().await;
    let name: &'static str = Box::leak(id.to_owned().into_boxed_str());
    for step in ["stop", "start"] {
        let reply = api.post(&format!("/api/workspaces/{id}/{step}"), "").await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let end = events.until(ended(name), Duration::from_secs(600)).await;
        assert_eq!(end["step"], "done", "{end}");
    }

    socket_is_the_session_users(&vm, &workspace, server.port()).await;

    // The first signature for this workspace, host and key waits for the user: nothing is signed
    // before the answer, and "always" lets the login through.
    let ((code, out), ()) = tokio::join!(
        login(&vm, &workspace, server.port()),
        answer_first_use(&api, id, &key, "always")
    );
    eprintln!("{out}");
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("logged in"), "{out}");
    let bound = out.find("bound agent to hostkey").unwrap_or_else(|| {
        panic!("ssh did not bind the agent to the session: {out}");
    });
    let offered = out.find("Offering public key").unwrap();
    assert!(bound < offered, "the bind comes before the key list: {out}");
    assert_eq!(
        *logins.lock().unwrap(),
        [key.fingerprint()],
        "the server saw the user's key, signed"
    );
    assert_eq!(
        asked.lock().unwrap().as_slice(),
        ["list", "sign"],
        "the host's agent listed once and signed once"
    );

    // A second login with the same key asks nothing: "always" stands.
    let (code, out) = login(&vm, &workspace, server.port()).await;
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        logins.lock().unwrap().len(),
        2,
        "the standing answer let it in"
    );
    let list = api.get("/api/ssh-agent-asks").await;
    assert_eq!(list.body, r#"{"asks":[]}"#, "no second question");
    let signed_before_off = asked
        .lock()
        .unwrap()
        .iter()
        .filter(|a| **a == "sign")
        .count();
    assert_eq!(signed_before_off, 2, "{:?}", asked.lock().unwrap());

    // Off again: the filter reads the switch on every request, so the socket stays until the next
    // start but hands out no key.
    let off = api
        .delete(&format!("/api/workspaces/{id}/ssh-agent/{NAME}"), None)
        .await;
    assert_eq!(off.status, 204, "{}", off.body);
    let (code, out) = login(&vm, &workspace, server.port()).await;
    eprintln!("{out}");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("Permission denied"), "{out}");
    assert_eq!(logins.lock().unwrap().len(), 2, "no third login");
    assert_eq!(
        asked
            .lock()
            .unwrap()
            .iter()
            .filter(|a| **a == "sign")
            .count(),
        signed_before_off,
        "nothing signed once off: {:?}",
        asked.lock().unwrap()
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// A server that offers a user's key no login: it records the keys offered and the keys that
/// proved themselves, and rejects every one. It stands for GitHub.
struct Closed {
    offered: Arc<Mutex<Vec<String>>>,
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the trait's methods are async; the server answers at once"
)]
impl Handler for Closed {
    type Error = russh::Error;

    async fn auth_publickey_offered(
        &mut self,
        _user: &str,
        offer: PublicKeyOffer<'_>,
    ) -> Result<Auth, Self::Error> {
        self.offered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(
                offer
                    .public_key
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string(),
            );
        Ok(Auth::reject())
    }

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        self.offered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(
                key.public_key
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string(),
            );
        Ok(Auth::reject())
    }
}

/// A jump host: the `Server` login, then every `direct-tcpip` channel is joined to `behind`
/// whatever address it names; the addresses asked for go to `asked_for`.
struct Jump {
    login: Server,
    behind: std::net::SocketAddr,
    asked_for: Arc<Mutex<Vec<String>>>,
}

impl Handler for Jump {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        user: &str,
        key: VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        self.login.auth_publickey(user, key).await
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.login
            .channel_open_session(channel, reply, session)
            .await
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.login.exec_request(channel, data, session).await
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host: &str,
        port: u32,
        _originator: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.asked_for
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{host}:{port}"));
        let behind = self.behind;
        reply.accept().await;
        tokio::spawn(async move {
            if let Ok(mut onward) = tokio::net::TcpStream::connect(behind).await {
                let mut through = channel.into_stream();
                let _ = tokio::io::copy_bidirectional(&mut through, &mut onward).await;
            }
        });
        Ok(())
    }
}

fn server_config(seed: u8) -> Arc<Config> {
    Arc::new(Config {
        keys: vec![russh::keys::PrivateKey::from(
            russh::keys::ssh_key::private::Ed25519Keypair::from_seed(&[seed; 32]),
        )],
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        inactivity_timeout: Some(Duration::from_secs(120)),
        auth_rejection_time: Duration::ZERO,
        ..Config::default()
    })
}

/// `closed` on a local port: the stand-in for GitHub, which never lets a login in.
async fn closed_server(offered: Arc<Mutex<Vec<String>>>) -> std::net::SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = server_config(10);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let server = Closed {
                offered: Arc::clone(&offered),
            };
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, stream, server).await {
                    let _ = session.await;
                }
            });
        }
    });
    addr
}

/// A jump host on a local port that forwards to `behind`.
async fn jump_server(
    logins: Arc<Mutex<Vec<String>>>,
    asked_for: Arc<Mutex<Vec<String>>>,
    behind: std::net::SocketAddr,
) -> std::net::SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = server_config(11);
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let server = Jump {
                login: Server {
                    logins: Arc::clone(&logins),
                },
                behind,
                asked_for: Arc::clone(&asked_for),
            };
            let config = Arc::clone(&config);
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, stream, server).await {
                    let _ = session.await;
                }
            });
        }
    });
    addr
}

/// With the agent on for the jump host only, the guest's own `ssh` reaches GitHub three ways
/// and gets no signature from the user's key on any of them: `ssh -J` through the jump host (the
/// inner session's key exchange is inside the jump host's encrypted channel, so Puddle never
/// saw it and the agent has no session to bind it to), a GitHub address literal, and an alias
/// from the guest's own ssh config (the agent is on for neither of those hosts). The jump host
/// itself still logs in with the key, so the key works and the refusal is the filter's, and the
/// audit log says why.
#[expect(clippy::too_many_lines, reason = "one story, read top to bottom")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_hostile_hg53_the_guests_own_jump_literal_and_alias_logins_to_github_get_no_signature() {
    let key = Arc::new(TestKey::ed25519(4));
    let asked: Asked = Arc::default();
    let (_agent_root, agent_path) = user_agent(Arc::clone(&key), Arc::clone(&asked));
    let jump_logins = Arc::new(Mutex::new(Vec::new()));
    let jump_asked_for = Arc::new(Mutex::new(Vec::new()));
    let github_offered = Arc::new(Mutex::new(Vec::new()));
    let github = closed_server(Arc::clone(&github_offered)).await;
    let jump = jump_server(
        Arc::clone(&jump_logins),
        Arc::clone(&jump_asked_for),
        github,
    )
    .await;
    let proxy = NameProxy::start(&[
        (JUMP, jump),
        (GITHUB_LITERAL, github),
        (GITHUB_NAME, github),
    ])
    .await;
    let options = with_fake_names(
        through_company_proxy(support::named_id_options(), &proxy),
        [JUMP],
    );
    let vm = start_vm_host_platform("hg53", options, &HarnessPlatform::default(), |_| {}).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    vm.host
        .settings()
        .save_global(json!({"schema_version": 2, "ui": {"ssh_agent_pipe": agent_path}}))
        .unwrap();
    let workspace = create_workspace_from(
        &vm,
        &api,
        "hg53",
        &[JUMP, GITHUB_LITERAL, GITHUB_NAME],
        json!({"memory_mib": 2048}),
    )
    .await;
    let id = workspace.as_str();
    let on = api
        .put(
            &format!("/api/workspaces/{id}/ssh-agent/{JUMP}"),
            &json!({"keys": null}).to_string(),
        )
        .await;
    assert_eq!(on.status, 200, "{}", on.body);
    let mut events = api.events().await;
    let name: &'static str = Box::leak(id.to_owned().into_boxed_str());
    for step in ["stop", "start"] {
        let reply = api.post(&format!("/api/workspaces/{id}/{step}"), "").await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        let end = events.until(ended(name), Duration::from_secs(600)).await;
        assert_eq!(end["step"], "done", "{end}");
    }
    let jump_port = jump.port();
    let github_port = github.port();
    // The guest's own ssh config: the options every ssh needs, including the one `-J` starts for
    // the jump host, which takes none from the command line, and the alias. No known hosts at
    // all: the image's system file holds GitHub's real keys, which the stand-in does not have.
    let (code, out) = guest_as(
        &vm.runtime,
        &workspace,
        "agent",
        &format!(
            "mkdir -p ~/.ssh && printf '%s\\n' \
             'Host github-alias' '  HostName {GITHUB_NAME}' '  Port {github_port}' \
             'Host *' '  BatchMode yes' '  StrictHostKeyChecking accept-new' \
             '  UserKnownHostsFile /dev/null' '  GlobalKnownHostsFile /dev/null' '  ConnectTimeout 30' > ~/.ssh/config"
        ),
        Duration::from_secs(60),
    )
    .await;
    assert_eq!(code, 0, "{out}");
    let ssh = "ssh -v";

    // The user's key works at the jump host: ssh binds, lists, one signature after the answer.
    let jump_login = format!("{ssh} -p {jump_port} tester@{JUMP} run 2>&1");
    let ((code, out), ()) = tokio::join!(
        guest_as(
            &vm.runtime,
            &workspace,
            "agent",
            &jump_login,
            Duration::from_secs(120),
        ),
        answer_first_use_for(&api, id, &key, JUMP, "always")
    );
    eprintln!("{out}");
    assert_eq!(code, 0, "{out}");
    assert_eq!(*jump_logins.lock().unwrap(), [key.fingerprint()]);
    let signed_at_jump = asked.lock().unwrap().clone();
    assert_eq!(signed_at_jump, ["list", "sign"], "{signed_at_jump:?}");

    // `ssh -J`: the jump login signs again (the standing answer), the inner login to GitHub
    // never reaches the user's agent.
    let (code, out) = guest_as(
        &vm.runtime,
        &workspace,
        "agent",
        &format!("{ssh} -J tester@{JUMP}:{jump_port} -p 22 git@github.com true 2>&1"),
        Duration::from_secs(120),
    )
    .await;
    eprintln!("{out}");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("Permission denied"), "{out}");
    assert_eq!(
        jump_asked_for.lock().unwrap().as_slice(),
        ["github.com:22"],
        "the inner session went through the jump host"
    );
    assert_eq!(
        *jump_logins.lock().unwrap(),
        [key.fingerprint(), key.fingerprint()],
        "only the jump login was signed"
    );

    // A GitHub address literal and an alias from the guest's own config.
    for (via, target) in [
        ("literal", format!("-p {github_port} git@{GITHUB_LITERAL}")),
        ("alias", "git@github-alias".to_owned()),
    ] {
        let (code, out) = guest_as(
            &vm.runtime,
            &workspace,
            "agent",
            &format!("{ssh} {target} true 2>&1"),
            Duration::from_secs(120),
        )
        .await;
        eprintln!("{out}");
        assert_ne!(code, 0, "{via}: {out}");
        assert!(out.contains("Permission denied"), "{via}: {out}");
    }

    // GitHub saw no key from the user's agent on any path, and the agent signed only twice.
    assert_eq!(*github_offered.lock().unwrap(), Vec::<String>::new());
    let all = asked.lock().unwrap().clone();
    assert_eq!(
        all.iter().filter(|a| **a == "sign").count(),
        2,
        "the jump login twice and nothing else: {all:?}"
    );

    // The record says why.
    let page = api
        .get(&format!(
            "/api/audit?workspace={id}&type=workspace_op&limit=500"
        ))
        .await;
    // One record: the same refusal of a workspace is written once a minute, and every one of
    // the three logins was refused for the same reason.
    assert!(
        page.body.contains("a session no spliced connection showed"),
        "no record saying why: {}",
        page.body
    );
    assert!(
        !page
            .body
            .contains(&format!("signed a login to {GITHUB_NAME}"))
            && !page
                .body
                .contains(&format!("signed a login to {GITHUB_LITERAL}"))
            && !page.body.contains("signed a login to github.com"),
        "{}",
        page.body
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
