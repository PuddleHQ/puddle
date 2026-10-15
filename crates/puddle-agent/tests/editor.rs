// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests for the editor server: the agent starts the release's `bin/code-server` when the host
//! asks, hands it a fresh password it also writes to an owner-only file, and starts it again when
//! it dies. A shell script stands in for code-server and records what it was started with.
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
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
    let password_file = dir.path().join("run/editor-password");
    let mut config = Config::default();
    config.listen = "127.0.0.1:0".parse().unwrap();
    config.target = Target::Unix(dir.path().join("no-host.sock"));
    config.oom = None;
    config.bridge = None;
    config.editor = Some(EditorConfig {
        server,
        password_file: password_file.clone(),
    });
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
    assert!(args.contains("--bind-addr\n127.0.0.1:41337\n"), "{args}");
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
async fn a_server_that_dies_is_started_again_with_a_new_password() {
    let f = fixture();
    let _agent = Agent::start(f.config.clone()).await.unwrap();
    until("the first start", || f.out.join("1.pid").exists()).await;
    let first = read(&f.out, "1.password");
    kill(&read(&f.out, "1.pid"));
    until("the second start", || f.out.join("2.pid").exists()).await;
    let second = read(&f.out, "2.password");
    assert_ne!(first, second);
    assert_eq!(std::fs::read_to_string(&f.password_file).unwrap(), second);
}

#[tokio::test]
async fn without_the_hosts_request_no_server_starts() {
    let mut f = fixture();
    f.config.editor = None;
    let _agent = Agent::start(f.config.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!f.out.join("1.pid").exists());
    assert!(!f.password_file.exists());
}
