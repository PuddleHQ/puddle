// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's `host-control` messages through a real agent and the host's session code over a
//! Unix socket: the host sends "end the extension hosts", the agent ends them, and the reply says
//! so. Shell scripts below a fake release folder stand in for the editor server, its extension
//! host and its pty host.
#![cfg(target_os = "linux")]
#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use crate::common;

use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use puddle_agent::config::Target;
use puddle_agent::editor::EditorConfig;
use puddle_agent::{Agent, Config};
use puddle_agent_proto::editor::EditorServer;
use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::host::{GuestStream, HostConfig, StreamHandler, serve_session};
use puddle_agent_proto::host_control::{HostMessage, HostReply};
use tokio::net::UnixListener;
use tokio::process::{Child, Command};

#[derive(Clone, Default)]
struct Host(Arc<Mutex<Option<GuestLink>>>);

impl StreamHandler for Host {
    async fn handle(&self, _stream: GuestStream) {}

    async fn guest_link(&self, link: GuestLink) {
        *self.0.lock().unwrap() = Some(link);
    }
}

/// The server, as the agent runs it: waits to be killed.
const SERVER: &str = "#!/bin/sh\nexec sleep 600\n";
/// An extension host with a language server below it and a debuggee that ignores `TERM`.
const EXT_HOST: &str = "#!/bin/sh
sleep 600 &
sh -c 'trap \"\" TERM; sleep 600' \"$MARK\" &
wait
";
/// A pty host with a terminal job.
const PTY_HOST: &str = "#!/bin/sh\nsh -c 'sleep 600' &\nwait\n";

struct Rig {
    host: Host,
    release: std::path::PathBuf,
    agent: Agent,
    _dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let release = dir.path().join("release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    std::fs::create_dir_all(release.join("out")).unwrap();
    for (path, script) in [
        ("bin/code-server", SERVER),
        ("out/ext", EXT_HOST),
        ("out/pty", PTY_HOST),
    ] {
        let file = release.join(path);
        std::fs::write(&file, script).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let socket = dir.path().join("route.sock");
    let host = Host::default();
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = host.clone();
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let _ = serve_session(
                    conn,
                    common::workspace(),
                    Arc::new(common::ChanSink(tx)),
                    Arc::new(handler),
                    HostConfig::default(),
                )
                .await;
            });
        }
    });
    let mut server = EditorServer::new(
        dir.path().join("data").to_string_lossy(),
        dir.path().join("work").to_string_lossy(),
        120,
    );
    server.dir = release.to_string_lossy().into_owned();
    server.port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        target: Target::Unix(socket),
        oom: None,
        bridge: None,
        editor: Some(EditorConfig {
            server,
            password_file: dir.path().join("run/editor-password"),
        }),
        ..Config::default()
    };
    let agent = Agent::start(config).await.unwrap();
    Rig {
        host,
        release,
        agent,
        _dir: dir,
    }
}

impl Rig {
    async fn link(&self) -> GuestLink {
        for _ in 0..1000 {
            if let Some(link) = self.host.0.lock().unwrap().clone() {
                return link;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the agent never said hello");
    }

    /// Starts one of the fake server's own processes, through `sh` (a script another test just
    /// wrote may still be open for writing in a forked child).
    fn start(&self, script: &str, arg: &str) -> Child {
        Command::new("sh")
            .arg(self.release.join("out").join(script))
            .arg(arg)
            .env("MARK", self.mark())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn mark(&self) -> String {
        self.release
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// Whether the debuggees of this rig still run.
    fn debuggees(&self) -> usize {
        let mark = self.mark();
        puddle_agent::exthost::snapshot()
            .iter()
            .filter(|p| !p.zombie && p.argv.contains(&mark))
            .count()
    }
}

fn alive(pid: u32) -> bool {
    let pid = i32::try_from(pid).unwrap();
    puddle_agent::exthost::snapshot()
        .iter()
        .any(|p| p.pid == pid && !p.zombie)
}

#[tokio::test]
async fn the_hosts_message_ends_the_extension_host_and_its_debuggee_and_leaves_the_pty_host() {
    let rig = rig().await;
    let link = rig.link().await;
    let ext = rig.start("ext", "--type=extensionHost");
    let pty = rig.start("pty", "--type=ptyHost");
    let deadline = Instant::now() + Duration::from_secs(10);
    while rig.debuggees() < 1 {
        assert!(Instant::now() < deadline, "the fake host did not start");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (ext_id, pty_id) = (ext.id().unwrap(), pty.id().unwrap());

    let reply = link.send(&HostMessage::EndExtensionHosts).await.unwrap();

    match reply {
        HostReply::Done {
            hosts, processes, ..
        } => {
            assert_eq!(hosts, 1);
            assert!(processes >= 3, "{processes}");
        }
        other => panic!("unexpected reply {other:?}"),
    }
    assert!(!alive(ext_id));
    assert_eq!(rig.debuggees(), 0);
    assert!(alive(pty_id), "the pty host was ended");
    // A second message is a no-op that still answers.
    let again = link.send(&HostMessage::EndExtensionHosts).await.unwrap();
    assert_eq!(again, HostReply::done(0, 0, 0));
    end(pty).await;
}

#[tokio::test]
async fn a_message_type_the_agent_does_not_know_is_answered_as_ignored_and_does_nothing() {
    let rig = rig().await;
    let link = rig.link().await;
    let ext = rig.start("ext", "--type=extensionHost");
    let reply = link.send(&HostMessage::Unknown).await.unwrap();
    assert_eq!(reply, HostReply::Ignored);
    assert!(alive(ext.id().unwrap()));
    end(ext).await;
}

#[tokio::test]
async fn a_message_to_an_agent_that_is_gone_is_an_error_not_a_hang() {
    let rig = rig().await;
    let link = rig.link().await;
    drop(rig.agent);
    let sent = tokio::time::timeout(
        Duration::from_secs(25),
        link.send(&HostMessage::EndExtensionHosts),
    )
    .await;
    assert!(matches!(sent, Ok(Err(_))), "{sent:?}");
}

/// Ends a process of the test and everything below it (the scripts leave `sleep`s behind).
async fn end(mut child: Child) {
    let id = i32::try_from(child.id().unwrap()).unwrap();
    let below = puddle_agent::exthost::tree(&puddle_agent::exthost::snapshot(), &[id]);
    let _ = child.kill().await;
    for pid in below {
        let _ = std::process::Command::new("kill")
            .arg("-KILL")
            .arg(pid.to_string())
            .status();
    }
}
