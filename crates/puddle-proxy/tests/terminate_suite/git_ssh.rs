// SPDX-License-Identifier: GPL-3.0-or-later
//! Git over SSH, the guest's side: a real SSH client (russh) through the agent's route, the real
//! proxy and the real Git injector, to a recording upstream. What the client may do, what the
//! table says about each repository, what the record says, and the limits.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::git_support::GitRig;
use crate::terminate_support::{Guest, Handler, Pki, Recorded, Reply, RigBuilder};

use std::borrow::Cow;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use puddle_agent_proto::ssh::{PROTOCOL_HEADER, SSH};
use puddle_proxy::{
    BoxFuture, GitSsh, GitSshAccount, GitSshAccounts, GitSshCommand, GitSshContext, GitSshFailure,
    GitSshGreeting, GitSshHostKey, GitSshHostKeys, GitSshHttps, GitSshLimits, GitSshStream,
    GitSshUpstream,
};
use puddle_types::{ConnectionDecision, ConnectionEvent, ConnectionReason, Host, WorkspaceId};
use russh::ChannelMsg;
use russh::Preferred;
use russh::client::{self, Handle};
use russh::keys::PublicKeyOrCertificate;
use russh::keys::ssh_key::{Algorithm, EcdsaCurve, HashAlg, rand_core};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

/// The operating system's random numbers, for the host key of a test.
struct Os;

impl rand_core::TryRng for Os {
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut bytes = [0; 4];
        getrandom::fill(&mut bytes).unwrap();
        Ok(u32::from_le_bytes(bytes))
    }

    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes).unwrap();
        Ok(u64::from_le_bytes(bytes))
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        getrandom::fill(dst).unwrap();
        Ok(())
    }
}

impl rand_core::TryCryptoRng for Os {}

fn host_key() -> GitSshHostKey {
    GitSshHostKey::random(&mut Os, Algorithm::Ed25519).unwrap()
}

/// An upstream that records what it was asked and plays a small Git host: it says which command
/// it got, then counts what the guest sends until the guest is done.
#[derive(Debug, Default)]
struct Recording {
    calls: Mutex<Vec<GitSshCommand>>,
    fail: Mutex<Option<String>>,
    greet: bool,
}

impl Recording {
    fn calls(&self) -> Vec<GitSshCommand> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl GitSshUpstream for Recording {
    fn run<'a>(
        &'a self,
        _: &'a GitSshContext,
        command: &'a GitSshCommand,
        io: &'a mut dyn GitSshStream,
    ) -> BoxFuture<'a, Result<(), GitSshFailure>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(command.clone());
            if let Some(message) = self
                .fail
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
            {
                return Err(GitSshFailure::new(message));
            }
            let hello = format!("HELLO {} {}\n", command.service.command(), command.path);
            io.write_all(hello.as_bytes()).await.unwrap();
            let mut received = Vec::new();
            io.read_to_end(&mut received).await.unwrap();
            io.write_all(format!("GOT {} bytes\n", received.len()).as_bytes())
                .await
                .unwrap();
            Ok(())
        })
    }

    fn greeting<'a>(
        &'a self,
        _: &'a GitSshContext,
        host: &'a Host,
    ) -> BoxFuture<'a, GitSshGreeting> {
        if self.greet {
            return Box::pin(std::future::ready(GitSshGreeting {
                message: format!("Hi octocat! You have authenticated to {host}.\n"),
                status: 1,
            }));
        }
        Box::pin(std::future::ready(GitSshGreeting {
            message: format!("puddle: no identity for {host}\n"),
            status: 255,
        }))
    }
}

/// A workspace with the Git rig of the HTTPS tests and Git over SSH on top of it.
struct SshRig {
    git: GitRig,
    upstream: Arc<Recording>,
    key: GitSshHostKey,
}

impl SshRig {
    async fn new() -> Self {
        Self::with(Recording::default(), GitSshLimits::default()).await
    }

    async fn with(upstream: Recording, limits: GitSshLimits) -> Self {
        Self::with_keys(upstream, limits, vec![host_key()]).await
    }

    /// The same with these host keys (at least one); `key` is the first.
    async fn with_keys(
        upstream: Recording,
        limits: GitSshLimits,
        keys: Vec<GitSshHostKey>,
    ) -> Self {
        let upstream = Arc::new(upstream);
        let git_ssh = GitSsh::new(
            Arc::clone(&upstream) as Arc<dyn GitSshUpstream>,
            keys.clone(),
        )
        .with_limits(limits);
        let host: Handler = Arc::new(|_: &Recorded| Reply::ok("never asked"));
        Self::serving(git_ssh, &keys, host, &[], upstream).await
    }

    /// A rig whose Git host answers with `host`, serving SSH with `git_ssh` and `keys`, where
    /// `allow` names more names the rules allow.
    async fn serving(
        git_ssh: GitSsh,
        keys: &[GitSshHostKey],
        host: Handler,
        allow: &[&'static str],
        upstream: Arc<Recording>,
    ) -> Self {
        Self::serving_ruled(
            git_ssh,
            keys,
            host,
            allow,
            upstream,
            puddle_proxy::RequestRules::default(),
        )
        .await
    }

    /// As [`Self::serving`], with rules on method and path for the workspace.
    async fn serving_ruled(
        git_ssh: GitSsh,
        keys: &[GitSshHostKey],
        host: Handler,
        allow: &[&'static str],
        upstream: Arc<Recording>,
        request_rules: puddle_proxy::RequestRules,
    ) -> Self {
        let key = keys.first().expect("at least one host key").clone();
        let world = GitRig::world();
        let pki = Pki::new();
        let server = crate::terminate_support::FakeServer::tls(
            pki.server_config("bound.test", crate::terminate_support::Flaw::None),
            host,
        )
        .await;
        let mut builder = RigBuilder::new(&pki)
            .name("bound.test", server.addr)
            .name("github.com", server.addr)
            .name("ssh.github.com", server.addr)
            .name("gitlab.com", server.addr)
            .name("ssh.dev.azure.com", server.addr)
            .name("elsewhere.test", server.addr)
            .injector(world.injector.clone())
            .request_rules(request_rules)
            .git_ssh(git_ssh);
        builder.allow.extend_from_slice(allow);
        let rig = builder.build();
        Self {
            git: GitRig {
                world,
                server,
                rig,
                stand_in: String::new(),
            },
            upstream,
            key,
        }
    }

    /// An SSH client to `authority` through a `CONNECT` the way the agent's `connect` makes it.
    async fn client(&self, guest: &mut Guest, authority: &str) -> Handle<Trusting> {
        self.try_client(guest, authority).await.unwrap()
    }

    async fn try_client(
        &self,
        guest: &mut Guest,
        authority: &str,
    ) -> Result<Handle<Trusting>, String> {
        let (handle, shown) = self
            .handshake(guest, authority, client::Config::default())
            .await?;
        assert_eq!(shown, self.key.public_key().to_openssh().unwrap());
        Ok(handle)
    }

    /// The handshake with `config`; the connection and the host key the server showed.
    async fn handshake(
        &self,
        guest: &mut Guest,
        authority: &str,
        config: client::Config,
    ) -> Result<(Handle<Trusting>, String), String> {
        self.handshake_as(guest, authority, config, true).await
    }

    /// As [`Self::handshake`]; `announced` says whether the `CONNECT` marks itself as SSH, as the
    /// agent's `connect` does (a captured client that waits for the server is not marked).
    async fn handshake_as(
        &self,
        guest: &mut Guest,
        authority: &str,
        config: client::Config,
        announced: bool,
    ) -> Result<(Handle<Trusting>, String), String> {
        let mut stream = guest.open_stream().await;
        let mark = if announced {
            format!("{PROTOCOL_HEADER}: {SSH}\r\n")
        } else {
            String::new()
        };
        stream
            .write_all(
                format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{mark}\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut status = String::new();
        reader.read_line(&mut status).await.unwrap();
        if !status.contains(" 200 ") {
            return Err(status);
        }
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        let seen = Arc::new(Mutex::new(None));
        let handler = Trusting {
            seen: Arc::clone(&seen),
        };
        let handle = client::connect_stream(Arc::new(config), reader, handler)
            .await
            .map_err(|e| e.to_string())?;
        let shown = seen
            .lock()
            .unwrap()
            .clone()
            .ok_or("the server showed no key")?;
        Ok((handle, shown))
    }

    async fn events(&self, count: usize) -> Vec<ConnectionEvent> {
        self.git.rig.events(count).await
    }
}

/// A client that takes the server's key (and checks it is the one Puddle serves).
struct Trusting {
    seen: Arc<Mutex<Option<String>>>,
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "the method of russh's `Handler` is async"
)]
impl client::Handler for Trusting {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if let PublicKeyOrCertificate::PublicKey { key, .. } = key {
            *self.seen.lock().unwrap() = Some(key.to_openssh().unwrap());
        }
        Ok(true)
    }
}

/// What a command printed and how it ended.
#[derive(Debug, Default)]
struct Ran {
    out: String,
    err: String,
    exit: Option<u32>,
    /// The replies to the requests sent before the command, in order (`true` = success).
    replies: Vec<bool>,
}

async fn exec(handle: &Handle<Trusting>, env: &[(&str, &str)], command: &str, stdin: &[u8]) -> Ran {
    let mut channel = handle.channel_open_session().await.unwrap();
    for (name, value) in env {
        channel.set_env(true, *name, *value).await.unwrap();
    }
    channel.exec(true, command).await.unwrap();
    if !stdin.is_empty() {
        channel.data(stdin).await.unwrap();
    }
    channel.eof().await.unwrap();
    collect(&mut channel, env.len() + 1).await
}

async fn collect(channel: &mut russh::Channel<client::Msg>, replies: usize) -> Ran {
    let mut ran = Ran::default();
    while let Ok(Some(message)) = tokio::time::timeout(Duration::from_secs(5), channel.wait()).await
    {
        match message {
            ChannelMsg::Data { data } => ran.out.push_str(&String::from_utf8_lossy(&data)),
            ChannelMsg::ExtendedData { data, ext: 1 } => {
                ran.err.push_str(&String::from_utf8_lossy(&data));
            }
            ChannelMsg::ExitStatus { exit_status } => ran.exit = Some(exit_status),
            ChannelMsg::Success if ran.replies.len() < replies => ran.replies.push(true),
            ChannelMsg::Failure if ran.replies.len() < replies => ran.replies.push(false),
            ChannelMsg::Close => break,
            _ => {}
        }
    }
    ran
}

async fn login(handle: &mut Handle<Trusting>) {
    assert!(handle.authenticate_none("git").await.unwrap().success());
}

// ---------------------------------------------------------------------------------------------
// The guest's side: login, commands, the table, the record.

#[tokio::test]
async fn a_listed_repository_is_read_and_written_and_the_record_says_who_with_what() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let fetch = exec(
        &ssh,
        &[("GIT_PROTOCOL", "version=2")],
        "git-upload-pack '/acme/web.git'",
        b"0014command=ls-refs\n",
    )
    .await;
    assert_eq!(fetch.exit, Some(0), "{fetch:?}");
    assert_eq!(
        fetch.out,
        "HELLO git-upload-pack /acme/web.git\nGOT 20 bytes\n"
    );
    assert_eq!(fetch.replies, [true, true]);
    let push = exec(&ssh, &[], "git-receive-pack 'acme/web'", b"pack").await;
    assert_eq!(push.exit, Some(0), "{push:?}");
    assert!(push.out.starts_with("HELLO git-receive-pack /acme/web.git"));
    let calls = t.upstream.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].host.to_string(), "bound.test");
    assert_eq!(calls[0].git_protocol.as_deref(), Some("version=2"));
    assert_eq!(calls[1].git_protocol, None);
    assert!(
        calls[0]
            .binding_id
            .as_deref()
            .is_some_and(|id| id.starts_with("identity:"))
    );
    drop(ssh);
    let events = t.events(1).await;
    let event = &events[0];
    assert_eq!(event.decision, ConnectionDecision::Allow);
    assert_eq!(event.port, 22);
    let line = event.http.as_ref().unwrap();
    assert_eq!(
        (line.method(), line.path()),
        ("git-upload-pack", "/acme/web")
    );
    assert!(event.injected);
    assert!(event.binding_id.is_some());
    assert!(event.bytes_up > 0 && event.bytes_down > 0);
    assert_eq!(t.git.raised(), []);
}

#[tokio::test]
async fn a_push_to_a_repository_off_the_list_ends_before_anything_goes_upstream() {
    let t = SshRig::new().await;
    t.git.world.switches(true, false);
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let refused = exec(&ssh, &[], "git-receive-pack 'acme/other.git'", b"").await;
    assert_eq!(refused.exit, Some(1), "{refused:?}");
    assert!(
        refused.err.starts_with(
            "puddle: push to bound.test/acme/other is not on this workspace's push list"
        ),
        "{refused:?}"
    );
    assert_eq!(refused.out, "");
    // A fetch of it is another switch and goes through.
    let fetched = exec(&ssh, &[], "git-upload-pack 'acme/other.git'", b"").await;
    assert_eq!(fetched.exit, Some(0), "{fetched:?}");
    assert_eq!(t.upstream.calls().len(), 1);
    assert_eq!(t.upstream.calls()[0].service.command(), "git-upload-pack");
    drop(ssh);
    let events = t.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Blocked);
    assert_eq!(events[0].reason, ConnectionReason::Refused("push_denied"));
    let line = events[0].http.as_ref().unwrap();
    assert_eq!(
        (line.method(), line.path()),
        ("git-receive-pack", "/acme/other")
    );
    assert!(
        t.git
            .raised()
            .iter()
            .any(|e| matches!(e, puddle_types::Event::GitAccessDenied { .. }))
    );
}

#[tokio::test]
async fn the_owner_in_the_path_chooses_the_identity_and_an_alias_changes_nothing() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    // The SSH user name is whatever the guest likes; the owner decides.
    assert!(
        ssh.authenticate_none("github-work")
            .await
            .unwrap()
            .success()
    );
    exec(&ssh, &[], "git-upload-pack 'acme/web'", b"").await;
    exec(&ssh, &[], "git-upload-pack 'someone/else'", b"").await;
    let calls = t.upstream.calls();
    assert_eq!(calls.len(), 2);
    assert_ne!(calls[0].binding_id, calls[1].binding_id);
}

#[tokio::test]
async fn any_login_method_is_accepted_and_none_of_it_is_kept() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut by_password = t.client(&mut guest, "bound.test:22").await;
    assert!(
        by_password
            .authenticate_password("git", "CANARY-PASSWORD")
            .await
            .unwrap()
            .success()
    );
    let mut by_key = t.client(&mut guest, "bound.test:22").await;
    let key = russh::keys::PrivateKey::random(&mut Os, Algorithm::Ed25519).unwrap();
    assert!(
        by_key
            .authenticate_publickey(
                "git",
                russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None)
            )
            .await
            .unwrap()
            .success()
    );
    let ran = exec(&by_key, &[], "git-upload-pack 'acme/web'", b"").await;
    assert_eq!(ran.exit, Some(0));
    let mut by_prompt = t.client(&mut guest, "bound.test:22").await;
    assert!(matches!(
        by_prompt
            .authenticate_keyboard_interactive_start("git", None)
            .await
            .unwrap(),
        client::KeyboardInteractiveAuthResponse::Success
    ));
    drop((by_password, by_key, by_prompt));
    assert!(!format!("{:?}", t.events(3).await).contains("CANARY-PASSWORD"));
    assert!(!crate::terminate_support::captured_logs().contains("CANARY-PASSWORD"));
}

#[tokio::test]
async fn commands_outside_the_grammar_are_refused_and_nothing_goes_upstream() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    for (command, code) in [
        (
            "git-upload-archive 'acme/web.git'",
            "ssh_command_unsupported",
        ),
        ("git upload-pack 'acme/web.git'", "ssh_command_unsupported"),
        ("; id", "ssh_command_unsupported"),
        ("sh -c id", "ssh_command_unsupported"),
        (
            "git-upload-pack 'acme/web.git' more",
            "ssh_command_unsupported",
        ),
        ("scp -t /tmp", "ssh_command_unsupported"),
        ("git-upload-pack '../etc'", "bad_git_path"),
        ("git-upload-pack 'acme/../web'", "bad_git_path"),
        ("git-upload-pack acme/web", "bad_git_path"),
        ("git-receive-pack 'acme/web;id'", "bad_git_path"),
    ] {
        let ran = exec(&ssh, &[], command, b"x").await;
        assert_eq!(ran.exit, Some(1), "{command}: {ran:?}");
        assert!(ran.err.starts_with("puddle: "), "{command}: {ran:?}");
        assert!(ran.err.contains(code), "{command}: {ran:?}");
        assert_eq!(ran.out, "", "{command}");
    }
    assert_eq!(t.upstream.calls().len(), 0);
    drop(ssh);
    let events = t.events(1).await;
    assert_eq!(
        events[0].reason,
        ConnectionReason::Refused("ssh_command_unsupported")
    );
}

#[tokio::test]
async fn a_failure_of_the_upstream_is_one_line_and_exit_status_1() {
    let t = SshRig::new().await;
    *t.upstream.fail.lock().unwrap() = Some("the Git host did not answer".to_owned());
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web'", b"").await;
    assert_eq!(ran.exit, Some(1));
    assert_eq!(ran.err, "puddle: the Git host did not answer\n");
}

#[tokio::test]
async fn git_lfs_falls_back_to_https_with_no_token_in_the_answer() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let transfer = exec(&ssh, &[], "git-lfs-transfer 'acme/web.git' upload", b"").await;
    assert_eq!(
        (transfer.exit, transfer.out.as_str(), transfer.err.as_str()),
        (Some(127), "", "")
    );
    let auth = exec(&ssh, &[], "git-lfs-authenticate 'acme/web.git' upload", b"").await;
    assert_eq!(auth.exit, Some(0), "{auth:?}");
    assert_eq!(
        auth.out,
        "{\"href\":\"https://bound.test/acme/web.git/info/lfs\"}\n"
    );
    assert!(!auth.out.contains("token"));
    t.git.world.switches(true, false);
    let off = exec(
        &ssh,
        &[],
        "git-lfs-authenticate 'acme/other.git' upload",
        b"",
    )
    .await;
    assert_eq!(off.exit, Some(1));
    assert!(off.err.contains("push to bound.test/acme/other"), "{off:?}");
    assert_eq!(t.upstream.calls().len(), 0);
}

#[tokio::test]
async fn a_shell_without_a_terminal_gets_the_upstreams_greeting_and_exit_status() {
    let t = SshRig::with(
        Recording {
            greet: true,
            ..Recording::default()
        },
        GitSshLimits::default(),
    )
    .await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let mut channel = ssh.channel_open_session().await.unwrap();
    channel.request_shell(true).await.unwrap();
    let ran = collect(&mut channel, 1).await;
    assert_eq!(ran.exit, Some(1));
    assert_eq!(
        ran.err,
        "Hi octocat! You have authenticated to bound.test.\n"
    );
}

// ---------------------------------------------------------------------------------------------
// What a session may not do.

#[tokio::test]
async fn a_terminal_a_subsystem_agent_x11_a_second_command_and_other_variables_are_refused() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;

    let mut pty = ssh.channel_open_session().await.unwrap();
    pty.request_pty(true, "xterm", 80, 24, 0, 0, &[])
        .await
        .unwrap();
    assert!(!collect_one(&mut pty).await);

    let mut sftp = ssh.channel_open_session().await.unwrap();
    sftp.request_subsystem(true, "sftp").await.unwrap();
    assert!(!collect_one(&mut sftp).await);

    let mut agent = ssh.channel_open_session().await.unwrap();
    agent.agent_forward(true).await.unwrap();
    assert!(!collect_one(&mut agent).await);

    let mut x11 = ssh.channel_open_session().await.unwrap();
    x11.request_x11(true, false, "MIT-MAGIC-COOKIE-1", "00", 0)
        .await
        .unwrap();
    assert!(!collect_one(&mut x11).await);

    let mut env = ssh.channel_open_session().await.unwrap();
    env.set_env(true, "LD_PRELOAD", "/tmp/x.so").await.unwrap();
    assert!(!collect_one(&mut env).await);
    env.set_env(true, "GIT_PROTOCOL", "version=9")
        .await
        .unwrap();
    assert!(!collect_one(&mut env).await);
    // A value that would do is not enough: only `GIT_PROTOCOL` is taken.
    env.set_env(true, "LD_PRELOAD", "version=2").await.unwrap();
    assert!(!collect_one(&mut env).await);
    env.set_env(true, "GIT_PROTOCOL", "version=2")
        .await
        .unwrap();
    assert!(collect_one(&mut env).await);

    let mut twice = ssh.channel_open_session().await.unwrap();
    twice
        .exec(true, "git-upload-pack 'acme/web'")
        .await
        .unwrap();
    twice
        .exec(true, "git-upload-pack 'acme/web'")
        .await
        .unwrap();
    // Once the command has started, `GIT_PROTOCOL` is too late.
    twice
        .set_env(true, "GIT_PROTOCOL", "version=2")
        .await
        .unwrap();
    twice.eof().await.unwrap();
    let ran = collect(&mut twice, 3).await;
    assert_eq!(ran.replies, [true, false, false], "{ran:?}");
    assert_eq!(ran.exit, Some(0));
    assert_eq!(t.upstream.calls().len(), 1);
}

/// Whether the next reply to a request on `channel` is a success.
async fn collect_one(channel: &mut russh::Channel<client::Msg>) -> bool {
    loop {
        let message = tokio::time::timeout(Duration::from_secs(5), channel.wait())
            .await
            .expect("an answer")
            .expect("the channel is open");
        match message {
            ChannelMsg::Success => return true,
            ChannelMsg::Failure => return false,
            _ => {}
        }
    }
}

#[tokio::test]
async fn forwarding_in_either_direction_is_refused_with_a_reason() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let direct = ssh
        .channel_open_direct_tcpip("127.0.0.1", 9, "127.0.0.1", 1234)
        .await;
    let err = direct.expect_err("a forward is refused").to_string();
    assert!(err.contains("AdministrativelyProhibited"), "{err}");
    assert!(ssh.tcpip_forward("127.0.0.1", 2222).await.is_err());
    assert!(
        ssh.channel_open_direct_streamlocal("/var/run/docker.sock")
            .await
            .is_err()
    );
    assert!(ssh.channel_open_x11("127.0.0.1", 6000).await.is_err());
    assert!(ssh.streamlocal_forward("/tmp/guest.sock").await.is_err());
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web'", b"").await;
    assert_eq!(ran.exit, Some(0), "the connection still works: {ran:?}");
    assert_eq!(t.upstream.calls().len(), 1);
    drop(ssh);
    let events = t.events(1).await;
    assert_eq!(
        events[0].reason,
        ConnectionReason::Refused("ssh_channel_refused")
    );
}

/// Every kind of forwarding and agent request is recorded as a refusal on its own connection,
/// so none of them passes unnoticed in the audit.
#[tokio::test]
async fn each_forwarding_request_is_recorded_as_a_channel_refusal() {
    let t = SshRig::new().await;
    for round in 0..6 {
        let mut guest = t.git.rig.guest();
        let mut ssh = t.client(&mut guest, "bound.test:22").await;
        login(&mut ssh).await;
        match round {
            0 => {
                let _ = ssh
                    .channel_open_direct_tcpip("127.0.0.1", 9, "127.0.0.1", 1234)
                    .await;
            }
            1 => {
                let _ = ssh.channel_open_direct_streamlocal("/var/run/x.sock").await;
            }
            2 => {
                let _ = ssh.channel_open_x11("127.0.0.1", 6000).await;
            }
            3 => {
                let _ = ssh.tcpip_forward("127.0.0.1", 2222).await;
            }
            4 => {
                let _ = ssh.streamlocal_forward("/tmp/guest.sock").await;
            }
            _ => {
                let mut channel = ssh.channel_open_session().await.unwrap();
                channel.agent_forward(true).await.unwrap();
                assert!(!collect_one(&mut channel).await);
            }
        }
        drop(ssh);
        let events = t.events(round + 1).await;
        assert_eq!(
            events[round].reason,
            ConnectionReason::Refused("ssh_channel_refused"),
            "round {round}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Which connections are served, and the limits.

#[tokio::test]
async fn ssh_to_a_name_that_is_not_a_git_host_is_not_served_but_spliced_to_the_real_host() {
    let t = SshRig::new().await;
    t.git
        .rig
        .policy
        .allow(&crate::terminate_support::host("elsewhere.test"));
    let mut guest = t.git.rig.guest();
    // The name goes to the rules, then upstream: Puddle's server never answers, so the server key
    // check of `try_client` is never reached and the client sees the real host's bytes.
    let refused = t.try_client(&mut guest, "elsewhere.test:22").await;
    assert!(refused.is_err_and(|e| !e.contains(" 403 ")));
    let events = t.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Allow);
    assert_eq!(t.upstream.calls().len(), 0, "no Git command was served");
}

#[tokio::test]
async fn a_git_host_the_rules_do_not_allow_is_not_served() {
    let t = SshRig::new().await;
    t.git
        .rig
        .policy
        .deny_host(&crate::terminate_support::host("bound.test"));
    let mut guest = t.git.rig.guest();
    assert!(t.try_client(&mut guest, "bound.test:22").await.is_err());
    assert_eq!(t.upstream.calls().len(), 0);
}

#[tokio::test]
async fn the_built_in_git_hosts_are_served_whether_or_not_an_identity_covers_them() {
    let t = SshRig::new().await;
    for name in [
        "github.com",
        "ssh.github.com",
        "gitlab.com",
        "ssh.dev.azure.com",
    ] {
        t.git
            .rig
            .policy
            .allow(&crate::terminate_support::host(name));
    }
    let mut guest = t.git.rig.guest();
    for (name, command, host, path) in [
        (
            "github.com:22",
            "git-upload-pack 'a/b.git'",
            "github.com",
            "/a/b.git",
        ),
        (
            "ssh.github.com:443",
            "git-upload-pack '/a/b'",
            "github.com",
            "/a/b.git",
        ),
        (
            "gitlab.com:22",
            "git-upload-pack 'g/s/p.git'",
            "gitlab.com",
            "/g/s/p.git",
        ),
        (
            "ssh.dev.azure.com:22",
            "git-upload-pack 'v3/org/My%20Proj/repo'",
            "dev.azure.com",
            "/org/My%20Proj/_git/repo",
        ),
    ] {
        let mut ssh = t.client(&mut guest, name).await;
        login(&mut ssh).await;
        let ran = exec(&ssh, &[], command, b"").await;
        assert_eq!(ran.exit, Some(0), "{name}: {ran:?}");
        let call = t.upstream.calls().pop().unwrap();
        assert_eq!(
            (call.host.to_string().as_str(), call.path.as_str()),
            (host, path)
        );
    }
}

#[tokio::test]
async fn a_guest_that_never_finishes_its_handshake_is_closed() {
    let limits = GitSshLimits {
        handshake: Duration::from_millis(300),
        ..GitSshLimits::default()
    };
    let t = SshRig::with(Recording::default(), limits).await;
    let guest = t.git.rig.guest();
    let mut stream = guest.open_stream().await;
    stream
        .write_all(
            format!(
                "CONNECT bound.test:22 HTTP/1.1\r\nHost: bound.test:22\r\n{PROTOCOL_HEADER}: {SSH}\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    // The 200 and Puddle's banner come, then the guest says nothing.
    let mut sink = Vec::new();
    // The close may come as a reset: what matters is that it comes.
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut sink))
        .await
        .expect("the proxy closes the connection");
    t.events(1).await;
}

#[tokio::test]
async fn a_connection_with_no_open_channel_is_closed_after_the_idle_limit() {
    let limits = GitSshLimits {
        idle: Duration::from_millis(300),
        ..GitSshLimits::default()
    };
    let t = SshRig::with(Recording::default(), limits).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    puddle_test_kit::eventually(
        "the idle connection is closed",
        Duration::from_secs(5),
        || ssh.is_closed(),
    )
    .await;
    t.events(1).await;
}

#[tokio::test]
async fn a_busy_connection_is_not_closed_by_the_idle_limit_and_channels_are_capped() {
    let limits = GitSshLimits {
        idle: Duration::from_millis(200),
        channels: 2,
        ..GitSshLimits::default()
    };
    let t = SshRig::with(Recording::default(), limits).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    // Two commands that wait for the guest's input keep two channels open.
    let mut first = ssh.channel_open_session().await.unwrap();
    first
        .exec(true, "git-upload-pack 'acme/web'")
        .await
        .unwrap();
    let second = ssh.channel_open_session().await.unwrap();
    second
        .exec(true, "git-upload-pack 'acme/web'")
        .await
        .unwrap();
    assert!(
        ssh.channel_open_session().await.is_err(),
        "a third channel is over the cap"
    );
    // flake-ok: an absence window; the idle limit is 300 ms and nothing signals that it did not fire.
    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(!ssh.is_closed(), "busy, so not idle");
    first.eof().await.unwrap();
    let ran = collect(&mut first, 1).await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    second.eof().await.unwrap();
}

/// Swallows the first `skip` bytes written to it: a client whose identification line already went
/// out with the `CONNECT`, as the agent's `connect` command sends it.
struct SkipWrites<S> {
    inner: S,
    skip: usize,
}

impl<S: tokio::io::AsyncRead + Unpin> tokio::io::AsyncRead for SkipWrites<S> {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for SkipWrites<S> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        data: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.skip > 0 {
            let n = self.skip.min(data.len());
            self.skip -= n;
            return std::task::Poll::Ready(Ok(n));
        }
        std::pin::Pin::new(&mut self.inner).poll_write(cx, data)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[tokio::test]
async fn the_server_names_itself_puddle_in_its_identification_line() {
    let t = SshRig::new().await;
    let guest = t.git.rig.guest();
    let mut stream = guest.open_stream().await;
    stream
        .write_all(
            format!(
                "CONNECT bound.test:22 HTTP/1.1\r\nHost: bound.test:22\r\n{PROTOCOL_HEADER}: {SSH}\r\n\r\nSSH-2.0-test\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        if line.starts_with("SSH-") {
            break;
        }
    }
    assert_eq!(line, "SSH-2.0-Puddle\r\n");
}

#[tokio::test]
async fn a_libssh2_client_is_asked_for_a_key_because_it_fails_a_login_with_none() {
    // libgit2 (cargo's built-in Git) reads an accepted `none` as "no authentication methods
    // succeeded"; any key will do.
    const BANNER: &str = "SSH-2.0-libssh2_1.11.1";
    let t = SshRig::new().await;
    let guest = t.git.rig.guest();
    let mut stream = guest.open_stream().await;
    stream
        .write_all(
            format!(
                "CONNECT bound.test:22 HTTP/1.1\r\nHost: bound.test:22\r\n{PROTOCOL_HEADER}: {SSH}\r\n\r\n{BANNER}\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
    }
    let config = client::Config {
        client_id: russh::SshId::Standard(BANNER.into()),
        ..client::Config::default()
    };
    let handler = Trusting {
        seen: Arc::new(Mutex::new(None)),
    };
    let skipping = SkipWrites {
        inner: reader,
        skip: BANNER.len() + 2,
    };
    let mut ssh = client::connect_stream(Arc::new(config), skipping, handler)
        .await
        .unwrap();
    assert!(!ssh.authenticate_none("git").await.unwrap().success());
    let key = russh::keys::PrivateKey::random(&mut Os, Algorithm::Ed25519).unwrap();
    assert!(
        ssh.authenticate_publickey(
            "git",
            russh::keys::PrivateKeyWithHashAlg::new(Arc::new(key), None)
        )
        .await
        .unwrap()
        .success()
    );
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web'", b"").await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
}

#[tokio::test]
async fn a_closed_channel_frees_its_place_and_a_second_shell_on_one_channel_is_refused() {
    let limits = GitSshLimits {
        channels: 1,
        ..GitSshLimits::default()
    };
    let t = SshRig::with(Recording::default(), limits).await;
    let guest = t.git.rig.guest();
    let mut guest = guest;
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let unused = ssh.channel_open_session().await.unwrap();
    assert!(
        ssh.channel_open_session().await.is_err(),
        "the one place is taken"
    );
    unused.close().await.unwrap();
    let mut shell = puddle_test_kit::eventually_async(
        "a closed channel frees its place",
        Duration::from_secs(5),
        || async { ssh.channel_open_session().await.ok() },
    )
    .await;
    shell.request_shell(true).await.unwrap();
    shell.request_shell(true).await.unwrap();
    let ran = collect(&mut shell, 2).await;
    assert_eq!(ran.replies, [true, false], "{ran:?}");
    assert_eq!(ran.exit, Some(255));
}

#[tokio::test]
async fn a_download_of_lfs_objects_asks_the_table_about_a_fetch() {
    let t = SshRig::new().await;
    t.git.world.switches(false, true);
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let off = exec(
        &ssh,
        &[],
        "git-lfs-authenticate 'acme/other.git' download",
        b"",
    )
    .await;
    assert_eq!(off.exit, Some(1), "{off:?}");
    assert!(
        off.err.contains("pull from bound.test/acme/other"),
        "{off:?}"
    );
    let on = exec(
        &ssh,
        &[],
        "git-lfs-authenticate 'acme/web.git' download",
        b"",
    )
    .await;
    assert_eq!(on.exit, Some(0), "{on:?}");
    drop(ssh);
    let events = t.events(1).await;
    assert_eq!(events[0].reason, ConnectionReason::Refused("pull_denied"));
    let line = events[0].http.as_ref().unwrap();
    assert_eq!(line.method(), "git-lfs-download");
}

#[tokio::test]
async fn bytes_that_are_not_ssh_end_the_connection_without_a_command() {
    let t = SshRig::new().await;
    let guest = t.git.rig.guest();
    let mut stream = guest.open_stream().await;
    stream
        .write_all(
            format!(
                "CONNECT bound.test:22 HTTP/1.1\r\nHost: bound.test:22\r\n{PROTOCOL_HEADER}: {SSH}\r\n\r\nGET / HTTP/1.1\r\n\r\n"
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut sink = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut sink))
        .await
        .expect("the proxy ends the connection");
    assert_eq!(t.upstream.calls().len(), 0);
    let events = t.events(1).await;
    assert!(events[0].http.is_none());
}

#[derive(Debug)]
struct Bare;

impl GitSshUpstream for Bare {
    fn run<'a>(
        &'a self,
        _: &'a GitSshContext,
        _: &'a GitSshCommand,
        _: &'a mut dyn GitSshStream,
    ) -> BoxFuture<'a, Result<(), GitSshFailure>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

#[test]
fn the_debug_output_names_the_number_of_keys_and_never_a_key() {
    let git = GitSsh::new(Arc::new(Bare), vec![host_key()]).with_limits(GitSshLimits {
        channels: 3,
        ..GitSshLimits::default()
    });
    let shown = format!("{git:?}");
    assert!(shown.contains("host_keys: 1"), "{shown}");
    assert!(shown.contains("channels: 3"), "{shown}");
}

fn never_asked() -> Handler {
    Arc::new(|_: &Recorded| Reply::ok("never asked"))
}

#[tokio::test]
async fn without_an_answer_of_its_own_a_shell_says_no_identity_for_the_https_host_and_ends_255() {
    let keys = vec![host_key()];
    let git = GitSsh::new(Arc::new(Bare), keys.clone());
    let t = SshRig::serving(
        git,
        &keys,
        never_asked(),
        &["ssh.github.com"],
        Arc::default(),
    )
    .await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "ssh.github.com:443").await;
    login(&mut ssh).await;
    let mut channel = ssh.channel_open_session().await.unwrap();
    channel.request_shell(true).await.unwrap();
    let ran = collect(&mut channel, 1).await;
    assert_eq!(ran.exit, Some(255));
    assert_eq!(
        ran.err,
        "puddle: no identity on this workspace has a credential for github.com; add one on the workspace's Git tab\n"
    );
}

// ---------------------------------------------------------------------------------------------
// The HTTPS leg: each command as the smart-HTTP requests `git` sends, through the termination.

/// Names one account for every workspace and host, and keeps who asked.
#[derive(Debug)]
struct Accounts {
    account: GitSshAccount,
    asked: Mutex<Vec<String>>,
}

impl GitSshAccounts for Accounts {
    fn account<'a>(
        &'a self,
        workspace: &'a WorkspaceId,
        host: &'a Host,
    ) -> BoxFuture<'a, GitSshAccount> {
        self.asked
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("{workspace} {host}"));
        Box::pin(std::future::ready(self.account.clone()))
    }
}

/// A smart-HTTP Git host with one repository, `acme/web`: protocol v2 when asked for it, v0
/// otherwise, and a push answered with the size of what it got.
fn smart_host() -> Handler {
    Arc::new(|seen: &Recorded| {
        let path = seen.target.split('?').next().unwrap_or_default();
        let v2 = seen
            .header("git-protocol")
            .is_some_and(|p| p.contains("version=2"));
        match (seen.method.as_str(), path) {
            ("GET", "/acme/web.git/info/refs") if v2 => {
                Reply::ok("000eversion 2\n0013ls-refs=unborn\n0000")
            }
            ("GET", "/acme/web.git/info/refs") => {
                let service = seen.target.rsplit('=').next().unwrap_or_default();
                Reply::ok(&crate::git_support::advert(service))
            }
            ("POST", "/acme/web.git/git-upload-pack") => {
                Reply::ok(&format!("0032{SHA} refs/heads/main\n0000"))
            }
            ("POST", "/acme/web.git/git-receive-pack") => {
                Reply::ok(&format!("GOT {} bytes", seen.body.len()))
            }
            _ => Reply::status(404, "", "Repository not found"),
        }
    })
}

const SHA: &str = crate::git_support::SHA;

impl SshRig {
    /// A workspace whose Git over SSH goes out through the HTTPS leg to `host`, greeting as
    /// `account`, with `allow` allowed on top of `bound.test`.
    async fn https(
        host: Handler,
        account: GitSshAccount,
        allow: &[&'static str],
    ) -> (Self, Arc<Accounts>) {
        let accounts = Arc::new(Accounts {
            account,
            asked: Mutex::new(Vec::new()),
        });
        let keys = vec![host_key()];
        let upstream = GitSshHttps::new(Arc::clone(&accounts) as Arc<dyn GitSshAccounts>);
        let git = GitSsh::new(Arc::new(upstream), keys.clone());
        (
            Self::serving(git, &keys, host, allow, Arc::default()).await,
            accounts,
        )
    }
}

#[tokio::test]
async fn the_https_leg_sends_a_v2_command_as_gits_requests_with_the_identitys_credential() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let ran = exec(
        &ssh,
        &[("GIT_PROTOCOL", "version=2")],
        "git-upload-pack 'acme/web.git'",
        b"0014command=ls-refs\n00000000",
    )
    .await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    assert_eq!(
        ran.out,
        format!("000eversion 2\n0013ls-refs=unborn\n00000032{SHA} refs/heads/main\n0000")
    );
    let seen = t.git.server.recorded();
    let asked: Vec<String> = seen
        .iter()
        .map(|r| format!("{} {}", r.method, r.target))
        .collect();
    assert_eq!(
        asked,
        [
            "GET /acme/web.git/info/refs?service=git-upload-pack",
            "POST /acme/web.git/git-upload-pack"
        ]
    );
    for request in &seen {
        assert_eq!(
            request.headers_named("authorization"),
            [crate::git_support::token_basic(crate::git_support::WORK)]
        );
        assert_eq!(request.header("git-protocol"), Some("version=2"));
    }
    assert_eq!(seen[1].body, b"0014command=ls-refs\n0000");
    assert_eq!(
        t.git
            .server
            .accepted
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one connection carries a command's requests"
    );
    drop(ssh);
    let events = t.events(1).await;
    let event = &events[0];
    assert_eq!(event.port, 22);
    assert!(event.injected);
    assert_eq!(event.resolved_ip, Some(t.git.server.addr.ip()), "{event:?}");
    assert_eq!(event.http.as_ref().unwrap().method(), "git-upload-pack");
}

#[tokio::test]
async fn a_push_a_rule_on_method_and_path_refuses_inside_the_https_leg_is_recorded_as_refused() {
    let rules = puddle_proxy::RequestRules::new(vec![
        puddle_proxy::RequestRule::new(
            puddle_types::RuleId(7),
            "bound.test",
            puddle_types::Methods::parse(["POST"]).unwrap(),
            puddle_types::PathPattern::parse("/acme/web.git/git-receive-pack").unwrap(),
        )
        .unwrap(),
    ]);
    let keys = vec![host_key()];
    let upstream = GitSshHttps::new(Arc::new(Accounts {
        account: GitSshAccount::Unnamed,
        asked: Mutex::new(Vec::new()),
    }) as Arc<dyn GitSshAccounts>);
    let t = SshRig::serving_ruled(
        GitSsh::new(Arc::new(upstream), keys.clone()),
        &keys,
        smart_host(),
        &[],
        Arc::default(),
        rules,
    )
    .await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let update =
        format!("{SHA} 2222222222222222222222222222222222222222 refs/heads/main\0report-status\n");
    let input = format!("{:04x}{update}0000", update.len() + 4).into_bytes();
    let ran = exec(&ssh, &[], "git-receive-pack 'acme/web.git'", &input).await;
    assert_ne!(ran.exit, Some(0), "the rule refused the push: {ran:?}");
    assert!(
        t.git.server.recorded().iter().all(|r| r.method != "POST"),
        "the push never reached the host"
    );
    drop(ssh);
    let events = t.events(1).await;
    let event = &events[0];
    assert_eq!(event.decision, ConnectionDecision::Deny, "{event:?}");
    assert_eq!(event.reason, ConnectionReason::RequestRule, "{event:?}");
    assert_eq!(event.request_rule, Some(puddle_types::RuleId(7)));
    assert_eq!(event.http.as_ref().unwrap().method(), "git-receive-pack");
}

#[tokio::test]
async fn the_https_leg_streams_a_push_and_returns_the_hosts_report() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let update =
        format!("{SHA} 2222222222222222222222222222222222222222 refs/heads/main\0report-status\n");
    let mut input = format!("{:04x}{update}0000", update.len() + 4).into_bytes();
    input.extend(std::iter::repeat_n(b'P', 300_000));
    let ran = exec(&ssh, &[], "git-receive-pack 'acme/web.git'", &input).await;
    assert_eq!(ran.exit, Some(0), "{ran:?}");
    assert!(
        ran.out.starts_with(&format!("0069{SHA} refs/heads/main\0")),
        "{ran:?}"
    );
    assert!(
        ran.out.ends_with(&format!("GOT {} bytes", input.len())),
        "{ran:?}"
    );
    let seen = t.git.server.recorded();
    assert_eq!(seen[1].body, input);
    assert_eq!(seen[1].header("transfer-encoding"), Some("chunked"));
}

#[tokio::test]
async fn a_host_that_answers_with_a_refusal_ends_the_command_with_its_status() {
    let (t, _) = SshRig::https(smart_host(), GitSshAccount::Unnamed, &[]).await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/gone.git'", b"").await;
    assert_eq!(ran.exit, Some(1));
    assert_eq!(
        ran.err,
        "puddle: the Git host answered 404 Not Found for bound.test/acme/gone.git\n"
    );
}

#[tokio::test]
async fn the_https_leg_reaches_only_a_host_the_rules_allow_and_puddle_decrypts() {
    let (t, _) = SshRig::https(
        smart_host(),
        GitSshAccount::Unnamed,
        &["gitlab.com", "ssh.github.com"],
    )
    .await;
    let mut guest = t.git.rig.guest();
    // gitlab.com is allowed but not decrypted for the workspace.
    let mut ssh = t.client(&mut guest, "gitlab.com:22").await;
    login(&mut ssh).await;
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(1));
    assert_eq!(
        ran.err,
        "puddle: Puddle does not decrypt gitlab.com for this workspace, so it cannot carry Git over SSH there\n"
    );
    // ssh.github.com is allowed, github.com (its HTTPS host) is not.
    let mut ssh = t.client(&mut guest, "ssh.github.com:443").await;
    login(&mut ssh).await;
    let ran = exec(&ssh, &[], "git-upload-pack 'acme/web.git'", b"").await;
    assert_eq!(ran.exit, Some(1));
    assert!(
        ran.err.starts_with("puddle: ") && ran.err.contains("github.com"),
        "{ran:?}"
    );
    assert!(t.git.server.recorded().is_empty());
}

#[tokio::test]
async fn ssh_t_through_the_https_leg_greets_the_account_puddle_would_use() {
    let (t, accounts) = SshRig::https(
        smart_host(),
        GitSshAccount::Named("octocat".to_owned()),
        &[],
    )
    .await;
    let mut guest = t.git.rig.guest();
    let mut ssh = t.client(&mut guest, "bound.test:22").await;
    login(&mut ssh).await;
    let mut channel = ssh.channel_open_session().await.unwrap();
    channel.request_shell(true).await.unwrap();
    let ran = collect(&mut channel, 1).await;
    assert_eq!(ran.exit, Some(1));
    assert_eq!(
        ran.err,
        "Hi octocat! You've successfully authenticated, but bound.test does not provide shell access.\n"
    );
    assert_eq!(*accounts.asked.lock().unwrap(), ["box bound.test"]);
}

#[tokio::test]
async fn a_client_that_waits_for_the_server_on_port_22_gets_puddles_server() {
    let t = SshRig::new().await;
    let mut guest = t.git.rig.guest();
    // `ssh-keyscan` sends nothing before the server's identification, so nothing marks it as SSH.
    let (_, shown) = t
        .handshake_as(
            &mut guest,
            "bound.test:22",
            client::Config::default(),
            false,
        )
        .await
        .unwrap();
    assert_eq!(shown, t.key.public_key().to_openssh().unwrap());
    // On another port such a connection is not taken for SSH.
    assert!(
        t.handshake_as(
            &mut guest,
            "bound.test:2222",
            client::Config::default(),
            false
        )
        .await
        .is_err()
    );
}

// ---------------------------------------------------------------------------------------------
// The host keys of one start.

/// One start's keys for the whole test binary: the RSA key takes a moment to make.
fn start_keys() -> &'static GitSshHostKeys {
    static KEYS: OnceLock<GitSshHostKeys> = OnceLock::new();
    KEYS.get_or_init(|| GitSshHostKeys::generate().unwrap())
}

#[tokio::test]
async fn a_client_gets_the_key_of_the_type_it_asks_for_and_the_known_hosts_lines_list_it() {
    let keys = start_keys();
    let t = SshRig::with_keys(
        Recording::default(),
        GitSshLimits::default(),
        keys.keys().to_vec(),
    )
    .await;
    let hosts = [Host::parse_normalised("bound.test").unwrap()];
    let listed = keys.known_hosts(&hosts).unwrap();
    for (algorithm, kind) in [
        (Algorithm::Ed25519, "ssh-ed25519"),
        (
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
            "ecdsa-sha2-nistp256",
        ),
        (
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            },
            "ssh-rsa",
        ),
    ] {
        let config = client::Config {
            preferred: Preferred {
                key: Cow::Owned(vec![algorithm]),
                ..Preferred::default()
            },
            ..client::Config::default()
        };
        let mut guest = t.git.rig.guest();
        let (mut handle, shown) = t
            .handshake(&mut guest, "bound.test:22", config)
            .await
            .unwrap();
        login(&mut handle).await;
        assert!(shown.starts_with(kind), "{kind}: {shown}");
        // What the client saw is a line of the file the guest's `ssh` reads, in both spellings.
        let shown = shown.split(' ').take(2).collect::<Vec<_>>().join(" ");
        for pattern in ["bound.test", "[bound.test]:443"] {
            let line = format!("{pattern} {shown}");
            assert!(listed.lines().any(|l| l == line), "{line} not in {listed}");
        }
        let ran = exec(&handle, &[], "git-upload-pack '/acme/web.git'", b"").await;
        assert!(ran.out.starts_with("HELLO git-upload-pack"), "{ran:?}");
    }
}

#[tokio::test]
async fn a_workspace_with_keys_of_its_own_start_is_served_with_them_and_not_the_default_ones() {
    let keys = Arc::new(start_keys().clone());
    let decoy = host_key();
    let upstream = Arc::new(Recording::default());
    let git_ssh = GitSsh::new(
        Arc::clone(&upstream) as Arc<dyn GitSshUpstream>,
        vec![decoy.clone()],
    );
    let world = GitRig::world();
    let pki = Pki::new();
    let server = crate::terminate_support::FakeServer::tls(
        pki.server_config("bound.test", crate::terminate_support::Flaw::None),
        Arc::new(|_: &Recorded| Reply::ok("never asked")),
    )
    .await;
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(world.injector.clone())
        .git_ssh(git_ssh)
        .git_ssh_keys(Arc::clone(&keys))
        .build();
    let t = SshRig {
        git: GitRig {
            world,
            server,
            rig,
            stand_in: String::new(),
        },
        upstream,
        key: keys.keys()[0].clone(),
    };
    let mut guest = t.git.rig.guest();
    let (mut handle, shown) = t
        .handshake(&mut guest, "bound.test:22", client::Config::default())
        .await
        .unwrap();
    login(&mut handle).await;
    assert_eq!(shown, keys.keys()[0].public_key().to_openssh().unwrap());
    assert_ne!(shown, decoy.public_key().to_openssh().unwrap());
}
