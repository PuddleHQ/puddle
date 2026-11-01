// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's `host-control` messages through a real agent and the host's session code over a
//! Unix socket: the host sends "end the extension hosts", the agent ends them, and the reply says
//! so. Shell scripts below a fake release folder stand in for the editor server, its extension
//! host and its pty host.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use crate::common;

use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_agent::config::Target;
use puddle_agent::editor::EditorConfig;
use puddle_agent::{Agent, Config};
use puddle_agent_proto::editor::EditorServer;
use puddle_agent_proto::forward::GuestLink;
use puddle_agent_proto::host::{GuestStream, HostConfig, StreamHandler, serve_session};
use puddle_agent_proto::host_control::{HostMessage, HostReply};
use puddle_test_kit::eventually;
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
    let mut server = EditorServer::new(
        dir.path().join("data").to_string_lossy(),
        dir.path().join("work").to_string_lossy(),
        120,
    );
    server.dir = release.to_string_lossy().into_owned();
    let (host, agent) = start_agent(&dir, server).await;
    Rig {
        host,
        release,
        agent,
        _dir: dir,
    }
}

/// An agent that runs the editor `server` and a host session it dials.
async fn start_agent(dir: &tempfile::TempDir, mut server: EditorServer) -> (Host, Agent) {
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
    server.port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    std::fs::write(dir.path().join("boot_id"), "this-boot\n").unwrap();
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        target: Target::Unix(socket),
        oom: None,
        bridge: None,
        editor: Some(EditorConfig {
            server,
            password_file: dir.path().join("run/editor-password"),
            boot_id: dir.path().join("boot_id"),
        }),
        ..Config::default()
    };
    let agent = Agent::start(config).await.unwrap();
    (host, agent)
}

impl Rig {
    async fn link(&self) -> GuestLink {
        eventually(
            "the agent never said hello",
            Duration::from_secs(10),
            || self.host.0.lock().unwrap().clone(),
        )
        .await
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
    eventually(
        "the fake host did not start",
        Duration::from_secs(10),
        || rig.debuggees() >= 1,
    )
    .await;
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

#[tokio::test]
async fn the_hosts_message_to_switch_builds_is_a_no_op_for_a_server_that_does_not_update() {
    let rig = rig().await;
    let link = rig.link().await;
    let reply = link.send(&HostMessage::SwitchEditorBuild).await.unwrap();
    assert_eq!(reply, HostReply::EditorBuild { switched: false });
}

const COMMIT_1: &str = "1111111111111111111111111111111111111111";
const COMMIT_2: &str = "2222222222222222222222222222222222222222";

/// A build of Microsoft's server that logs its name and waits to be ended.
fn build(builds: &std::path::Path, commit: &str, name: &str, log: &std::path::Path) {
    let program = builds.join(commit).join("bin/code-server");
    std::fs::create_dir_all(program.parent().unwrap()).unwrap();
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\necho {name} >>{}\nexec sleep 600\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(builds.join(commit).join(".puddle-installed"), "").unwrap();
}

#[tokio::test]
async fn the_hosts_message_runs_the_staged_build_and_a_second_message_has_nothing_left_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let builds = dir.path().join("data/servers");
    let log = dir.path().join("log");
    build(&builds, COMMIT_1, "first", &log);
    build(&builds, COMMIT_2, "second", &log);
    std::fs::write(builds.join("current"), COMMIT_1).unwrap();
    let mut server = EditorServer::microsoft(
        dir.path().join("data").to_string_lossy(),
        dir.path().join("work").to_string_lossy(),
        120,
        false,
        true,
    );
    server.dir = builds.to_string_lossy().into_owned();
    let (host, _agent) = start_agent(&dir, server).await;
    let link = eventually("the agent says hello", Duration::from_secs(30), || {
        host.0.lock().unwrap().clone()
    })
    .await;
    let logged = || std::fs::read_to_string(&log).unwrap_or_default();
    let until = |what: &'static str, want: &'static str| {
        let logged = &logged;
        async move {
            eventually(what, Duration::from_secs(30), || {
                let log = logged();
                if log == want {
                    Ok(())
                } else {
                    Err(format!("log is {log:?}"))
                }
            })
            .await;
        }
    };
    until("the first build to start", "first\n").await;
    // Nothing staged: the server runs on.
    let none = link.send(&HostMessage::SwitchEditorBuild).await.unwrap();
    assert_eq!(none, HostReply::EditorBuild { switched: false });
    // Staged (as the agent's update step leaves it): the message runs it.
    std::fs::write(builds.join("staged"), COMMIT_2).unwrap();
    let switched = link.send(&HostMessage::SwitchEditorBuild).await.unwrap();
    assert_eq!(switched, HostReply::EditorBuild { switched: true });
    until("the staged build to start", "first\nsecond\n").await;
    let again = link.send(&HostMessage::SwitchEditorBuild).await.unwrap();
    assert_eq!(again, HostReply::EditorBuild { switched: false });
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
