// SPDX-License-Identifier: GPL-3.0-or-later
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
use russh::server::{Auth, ChannelOpenHandle, Config, Handler, Msg, Session, VerifiedPublicKey};
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

    let (code, setup) = guest_as(
        &vm.runtime,
        &workspace,
        "agent",
        &format!(
            "for i in $(seq 50); do [ -S {SOCKET} ] && break; sleep 0.2; done; \
             echo \"owner=$(stat -c '%U %a' {SOCKET})\"; echo \"sock=$SSH_AUTH_SOCK\"; \
             ssh -G -p {} {NAME} | grep -i '^identityagent'",
            server.port()
        ),
        Duration::from_secs(60),
    )
    .await;
    eprintln!("{setup}");
    assert_eq!(code, 0, "{setup}");
    assert!(setup.contains("owner=agent 600"), "{setup}");
    assert!(setup.contains(&format!("sock={SOCKET}")), "{setup}");
    assert!(
        setup.contains(&format!("identityagent {SOCKET}")),
        "{setup}"
    );

    let (code, out) = login(&vm, &workspace, server.port()).await;
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
    assert_eq!(logins.lock().unwrap().len(), 1, "no second login");
    assert!(
        !asked.lock().unwrap()[2..].contains(&"sign"),
        "nothing signed once off: {:?}",
        asked.lock().unwrap()
    );

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
