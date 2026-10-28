// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests for the editor server: the agent starts the release's `bin/code-server` when the host
//! asks, hands it a fresh password it also writes to an owner-only file, and starts it again when
//! it dies. A shell script stands in for code-server and records what it was started with.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns run only in tests"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use puddle_agent::config::Target;
use puddle_agent::editor::EditorConfig;
use puddle_agent::{Agent, Config};
use puddle_agent_proto::editor::EditorServer;

const WAIT: Duration = Duration::from_secs(20);

/// Records its arguments, its `PASSWORD` and the number of `VSCODE_` variables it was started
/// with in `OUT/<start number>.*` (`OUT` is filled in), then waits to be killed.
const FAKE: &str = r#"#!/bin/sh
n=1
while [ -e "OUT/$n.pid" ]; do n=$((n + 1)); done
printf '%s\n' "$@" >"OUT/$n.args"
printf '%s' "$PASSWORD" >"OUT/$n.password"
env | grep -c '^VSCODE_' >"OUT/$n.vscode" || true
echo $$ >"OUT/$n.pid"
exec sleep 600
"#;

struct Fixture {
    _dir: tempfile::TempDir,
    out: PathBuf,
    config: Config,
    password_file: PathBuf,
}

/// A port nothing listens on now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let release = dir.path().join("release");
    std::fs::create_dir_all(release.join("bin")).unwrap();
    let script = release.join("bin/code-server");
    let out = dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    std::fs::write(&script, FAKE.replace("OUT", &out.to_string_lossy())).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut server = EditorServer::new(
        dir.path().join("data").to_string_lossy(),
        dir.path().join("work").to_string_lossy(),
        120,
    );
    server.dir = release.to_string_lossy().into_owned();
    server.port = free_port();
    let password_file = dir.path().join("run/editor-password");
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        target: Target::Unix(dir.path().join("no-host.sock")),
        oom: None,
        bridge: None,
        editor: Some(EditorConfig {
            server,
            password_file: password_file.clone(),
        }),
        ..Config::default()
    };
    Fixture {
        _dir: dir,
        out,
        config,
        password_file,
    }
}

async fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn read(out: &Path, name: &str) -> String {
    std::fs::read_to_string(out.join(name)).unwrap()
}

fn kill(pid: &str) {
    let status = std::process::Command::new("kill")
        .args(["-9", pid.trim()])
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn the_agent_starts_the_server_with_a_password_only_the_server_and_a_private_file_hold() {
    let f = fixture();
    let _agent = Agent::start(f.config.clone()).await.unwrap();
    until("the first start", || f.out.join("1.pid").exists()).await;

    let server = &f.config.editor.as_ref().unwrap().server;
    let args = read(&f.out, "1.args");
    assert!(
        args.contains(&format!("--bind-addr\n127.0.0.1:{}\n", server.port)),
        "{args}"
    );
    assert!(args.contains("--reconnection-grace-time\n120\n"), "{args}");
    assert!(args.trim_end().ends_with(&server.folder), "{args}");
    let password = read(&f.out, "1.password");
    assert_eq!(password.len(), 64);
    assert_eq!(std::fs::read_to_string(&f.password_file).unwrap(), password);
    assert!(
        !args.contains(&password),
        "the password is not on the command line"
    );
    let mode = std::fs::metadata(&f.password_file)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    for sub in ["user-data", "extensions"] {
        assert!(Path::new(&server.data).join(sub).is_dir(), "{sub} is made");
    }
    assert_eq!(read(&f.out, "1.vscode").trim(), "0");
}

#[tokio::test]
async fn a_server_that_dies_is_started_again_with_the_same_password() {
    let f = fixture();
    let _agent = Agent::start(f.config.clone()).await.unwrap();
    until("the first start", || f.out.join("1.pid").exists()).await;
    let first = read(&f.out, "1.password");
    kill(&read(&f.out, "1.pid"));
    until("the second start", || f.out.join("2.pid").exists()).await;
    // A window signed in with the first password stays signed in.
    assert_eq!(read(&f.out, "2.password"), first);
    assert_eq!(std::fs::read_to_string(&f.password_file).unwrap(), first);
}

#[tokio::test]
async fn a_restarted_server_opens_the_multi_root_file_that_an_attach_wrote_meanwhile() {
    let mut f = fixture();
    let editor = f.config.editor.as_mut().unwrap();
    let file = Path::new(&editor.server.folder).join("shop.code-workspace");
    editor.server = editor
        .server
        .clone()
        .with_workspace_file(file.to_string_lossy());
    let folder = editor.server.folder.clone();
    let _agent = Agent::start(f.config.clone()).await.unwrap();
    until("the first start", || f.out.join("1.pid").exists()).await;
    // Before any attach there is no file: the folder opens.
    assert!(read(&f.out, "1.args").trim_end().ends_with(&folder));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        &file,
        r#"{"folders": [{"path": "/workspaces/shop/api"}, {"path": "/workspaces/shop/web"}]}"#,
    )
    .unwrap();
    kill(&read(&f.out, "1.pid"));
    until("the second start", || f.out.join("2.pid").exists()).await;
    let args = read(&f.out, "2.args");
    assert!(
        args.trim_end().ends_with(&*file.to_string_lossy()),
        "{args}"
    );
    assert!(!args.contains(&format!("\n{folder}\n")), "{args}");
}

#[tokio::test]
async fn a_new_agent_leaves_a_running_server_alone_and_takes_over_when_it_is_gone() {
    let f = fixture();
    let port = f.config.editor.as_ref().unwrap().server.port;
    // Something answers on the port: the server an earlier agent left behind.
    let occupant = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    occupant.set_nonblocking(true).unwrap();
    let _agent = Agent::start(f.config.clone()).await.unwrap();
    // The agent's own look at the port is the signal; the short wait after it is for a start
    // that would follow a look that found the port free (a child process, a few ms).
    until("the agent to look at the port", || {
        occupant.accept().is_ok()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !f.out.join("1.pid").exists(),
        "a second server must not start on a taken port"
    );
    drop(occupant);
    until("the start once the port is free", || {
        f.out.join("1.pid").exists()
    })
    .await;
}

#[tokio::test]
async fn without_the_hosts_request_no_server_starts() {
    let mut f = fixture();
    f.config.editor = None;
    let _agent = Agent::start(f.config.clone()).await.unwrap();
    // Absence window: without the host's request no editor server starts.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!f.out.join("1.pid").exists());
    assert!(!f.password_file.exists());
}
