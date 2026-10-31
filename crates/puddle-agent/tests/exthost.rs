// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests for the extension-host cleanup with real processes: shell scripts below a fake server
//! folder stand in for the server's extension host, its pty host and the processes below them.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns run only in tests"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use puddle_agent::exthost::{self, Proc};
use puddle_test_kit::{eventually, eventually_blocking};
use tokio::process::{Child, Command};

/// An extension host with a language server below it, and (if `stubborn`) a debuggee that ignores
/// `TERM`. The script prints nothing and waits for its children.
const HOST: &str = r#"#!/bin/sh
sleep 600 &
if [ "$STUBBORN" = 1 ]; then sh -c 'trap "" TERM; sleep 600' "$MARK" & fi
wait
"#;

/// A pty host with a terminal's shell and a job below it.
const PTY: &str = "#!/bin/sh\nsh -c 'sleep 600' &\nwait\n";

struct Server {
    dir: tempfile::TempDir,
}

impl Server {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("out")).unwrap();
        for (name, script) in [("host", HOST), ("pty", PTY)] {
            let path = dir.path().join("out").join(name);
            std::fs::write(&path, script).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir }
    }

    /// The name the stubborn children of this server carry as `$0`: tells them from the other tests'.
    fn mark(&self) -> String {
        self.dir
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    /// This server's stubborn children that still run.
    fn stubborn(&self) -> Vec<Proc> {
        let mark = self.mark();
        exthost::snapshot()
            .into_iter()
            .filter(|p| !p.zombie && p.argv.contains(&mark))
            .collect()
    }

    fn folders(&self) -> Vec<PathBuf> {
        vec![self.dir.path().to_path_buf()]
    }

    fn start(&self, script: &str, arg: &str, stubborn: bool) -> Child {
        // Through `sh`, not by executing the file: a file another test just wrote may still be open for
        // writing in a forked child ("text file busy").
        Command::new("sh")
            .arg(self.dir.path().join("out").join(script))
            .arg(arg)
            .env("MARK", self.mark())
            .env("STUBBORN", if stubborn { "1" } else { "0" })
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    /// Every process, from the real process table.
    #[expect(clippy::unused_self, reason = "reads like the server's own view")]
    fn procs(&self) -> Vec<Proc> {
        exthost::snapshot()
    }
}

fn alive(pid: u32) -> bool {
    let pid = i32::try_from(pid).unwrap();
    exthost::snapshot()
        .iter()
        .any(|p| p.pid == pid && !p.zombie)
}

async fn settled(server: &Server) {
    // Wait until both scripts have started their children.
    eventually(
        "the fake processes did not start",
        Duration::from_secs(10),
        || {
            let tree_size = exthost::extension_hosts(&server.procs(), &server.folders()).len();
            let sleepers = exthost::snapshot()
                .iter()
                .filter(|p| p.argv.first().is_some_and(|a| a == "sleep") && p.ppid != 1)
                .count();
            tree_size >= 1 && sleepers >= 2
        },
    )
    .await;
}

#[tokio::test]
async fn the_extension_host_and_everything_below_it_end_in_under_a_second_and_the_pty_host_stays() {
    let server = Server::new();
    let host = server.start("host", "--type=extensionHost", false);
    let pty = server.start("pty", "--type=ptyHost", false);
    settled(&server).await;
    let (host_id, pty_id) = (host.id().unwrap(), pty.id().unwrap());

    let started = Instant::now();
    let ended = exthost::end_extension_hosts(&server.folders()).await;
    let took = started.elapsed();

    // flake-ok: the bar sits between ending on `TERM` (the agent polls `/proc` every 50 ms: 61 ms
    // measured under 16 busy loops on 4 cores) and the 3 s `TERM_GRACE` a `KILL` escalation waits.
    assert!(took < Duration::from_secs(1), "took {took:?}");
    assert_eq!(ended.hosts, 1);
    assert!(ended.processes >= 2, "{ended:?}");
    assert!(!alive(host_id), "the extension host is still running");
    assert!(alive(pty_id), "the pty host was ended");
    // The terminal's shell and job survive with it: two `sleep`s were below the pty host, one below the host.
    let sleepers = exthost::snapshot()
        .iter()
        .filter(|p| p.argv.first().is_some_and(|a| a == "sleep") && p.ppid != 1)
        .count();
    assert!(sleepers >= 1, "the terminal's job was ended");
    cleanup(pty).await;
}

#[tokio::test]
async fn a_process_that_ignores_term_is_killed_after_the_grace() {
    let server = Server::new();
    let host = server.start("host", "--type=extensionHost", true);
    settled(&server).await;
    let host_id = host.id().unwrap();
    // `settled` counts every test's `sleep`s (the process table is shared), so it can return before this
    // server's stubborn child has started: wait for that child itself.
    eventually(
        "the stubborn child started",
        Duration::from_secs(10),
        || !server.stubborn().is_empty(),
    )
    .await;
    let before: Vec<i32> = server.stubborn().iter().map(|p| p.pid).collect();
    assert_eq!(
        before.len(),
        1,
        "the stubborn child did not start: {before:?}"
    );

    let started = Instant::now();
    let ended = exthost::end_extension_hosts(&server.folders()).await;
    let took = started.elapsed();

    assert!(
        took >= exthost::TERM_GRACE,
        "ended before the grace: {took:?}"
    );
    assert!(
        took < exthost::TERM_GRACE + Duration::from_secs(3),
        "took {took:?}"
    );
    assert!(ended.processes >= 3, "{ended:?}");
    assert!(!alive(host_id));
    assert!(before.iter().all(|pid| {
        !exthost::snapshot()
            .iter()
            .any(|p| p.pid == *pid && !p.zombie)
    }));
}

#[tokio::test]
async fn twenty_hosts_each_with_a_debuggee_all_end_together() {
    let server = Server::new();
    let hosts: Vec<Child> = (0..20)
        .map(|_| server.start("host", "--type=extensionHost", true))
        .collect();
    eventually(
        "the fake hosts did not start",
        Duration::from_secs(20),
        || {
            exthost::extension_hosts(&server.procs(), &server.folders()).len() >= 20
                && server.stubborn().len() >= 20
        },
    )
    .await;
    let ids: Vec<u32> = hosts.iter().map(|h| h.id().unwrap()).collect();

    let ended = exthost::end_extension_hosts(&server.folders()).await;

    assert_eq!(ended.hosts, 20);
    assert!(ended.processes >= 60, "{ended:?}");
    assert_eq!(ids.iter().filter(|id| alive(**id)).count(), 0);
    let left = server.stubborn().len();
    assert_eq!(left, 0, "debuggees survived");
}

#[tokio::test]
async fn with_no_extension_host_there_is_nothing_to_end() {
    let server = Server::new();
    let pty = server.start("pty", "--type=ptyHost", false);
    let ended = exthost::end_extension_hosts(&server.folders()).await;
    assert_eq!((ended.hosts, ended.processes), (0, 0));
    assert!(alive(pty.id().unwrap()));
    cleanup(pty).await;
}

/// Debuggees started by a shell that exits at once, so this process (the subreaper) adopts them.
/// Each is `(socket it names, shell command, tag)`; returns their ids once all are adopted.
fn orphans(dir: &Path, specs: &[(PathBuf, &str, &str)]) -> Vec<i32> {
    // Orphans go to the nearest ancestor that is a subreaper, not to process 1: be that ancestor.
    rustix::process::set_child_subreaper(Some(rustix::process::getpid())).unwrap();
    for (sock, command, tag) in specs {
        let script = format!("({command} & echo $! >{}/{tag}.pid) &", dir.display());
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .env(
                "VSCODE_INSPECTOR_OPTIONS",
                format!(
                    "{{\"inspectorIpc\":\"{}\",\"deferredMode\":false}}",
                    sock.display()
                ),
            )
            .status()
            .unwrap();
    }
    let ids: Vec<i32> = specs
        .iter()
        .map(|(_, _, tag)| {
            eventually_blocking(
                "the shell wrote its pid file",
                Duration::from_secs(30),
                || {
                    let text = std::fs::read_to_string(dir.join(format!("{tag}.pid"))).ok()?;
                    text.trim().parse().ok()
                },
            )
        })
        .collect();
    let me = i32::try_from(std::process::id()).unwrap();
    eventually_blocking(
        "the orphans were not adopted",
        Duration::from_secs(10),
        || {
            ids.iter().all(|pid| {
                exthost::snapshot()
                    .iter()
                    .any(|p| p.pid == *pid && p.ppid == me)
            })
        },
    );
    ids
}

/// The orphan tests share one subreaper (this process) and each sweeps all of its orphans, so they
/// run one at a time.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn running(pid: i32) -> bool {
    exthost::snapshot()
        .iter()
        .any(|p| p.pid == pid && !p.zombie)
}

#[tokio::test]
async fn an_orphaned_debuggee_with_a_dead_socket_is_swept_and_one_with_a_live_socket_is_not() {
    let _only = ONE_AT_A_TIME.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live.sock");
    std::fs::write(&live, "").unwrap();
    let ids = orphans(
        dir.path(),
        &[
            (dir.path().join("dead.sock"), "sleep 601", "dead"),
            (live, "sleep 601", "live"),
        ],
    );
    let me = i32::try_from(std::process::id()).unwrap();

    let swept = exthost::sweep_orphans_of(me).await;

    let (dead_left, live_left) = (running(ids[0]), running(ids[1]));
    kill(ids[1]);
    assert_eq!(swept, 1);
    assert!(!dead_left, "the orphan with a dead socket survived");
    assert!(live_left, "the debuggee with a live socket was ended");
}

#[tokio::test]
async fn an_orphaned_debuggee_that_ignores_term_is_killed_after_the_grace() {
    let _only = ONE_AT_A_TIME.lock().await;
    let dir = tempfile::tempdir().unwrap();
    // An ignored signal stays ignored through `exec`, so the `sleep` shrugs off `TERM` too.
    let ready = dir.path().join("stubborn.ready");
    let command = format!("sh -c 'trap \"\" TERM; : >{}; sleep 602'", ready.display());
    let ids = orphans(
        dir.path(),
        &[(dir.path().join("dead.sock"), &command, "stubborn")],
    );
    // A `TERM` that came before the trap would end the shell at once and fake a pass.
    eventually_blocking(
        "the stand-in never ignored TERM",
        Duration::from_secs(10),
        || ready.exists(),
    );
    let me = i32::try_from(std::process::id()).unwrap();
    let started = Instant::now();

    let swept = exthost::sweep_orphans_of(me).await;

    let left = running(ids[0]);
    kill(ids[0]);
    assert_eq!(swept, 1);
    assert!(!left, "the orphan that ignores TERM survived");
    assert!(started.elapsed() >= exthost::TERM_GRACE);
}

#[tokio::test(start_paused = true)]
async fn the_sweep_loop_ends_an_orphan_when_its_time_comes_and_not_before() {
    let _only = ONE_AT_A_TIME.lock().await;
    let dir = tempfile::tempdir().unwrap();
    let ids = orphans(
        dir.path(),
        &[(dir.path().join("dead.sock"), "sleep 603", "dead")],
    );
    let me = i32::try_from(std::process::id()).unwrap();
    let looping = tokio::spawn(exthost::sweep_loop(me));

    tokio::time::sleep(exthost::SWEEP_EVERY / 2).await;
    let before = running(ids[0]);
    tokio::time::sleep(exthost::SWEEP_EVERY * 2).await;
    // The sweep's waits run on virtual time, the process dies in real time: give it that.
    let ended = std::panic::catch_unwind(|| {
        eventually_blocking(
            "the loop never ended the orphan",
            Duration::from_secs(5),
            || !running(ids[0]),
        );
    });
    looping.abort();
    kill(ids[0]);

    assert!(before, "the loop ended the orphan before its first sweep");
    if let Err(panic) = ended {
        std::panic::resume_unwind(panic);
    }
}

fn kill(pid: i32) {
    let _ = std::process::Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status();
}

/// Ends a process of the test and everything below it (the scripts leave `sleep`s that outlive them).
async fn cleanup(mut child: Child) {
    let below = exthost::tree(
        &exthost::snapshot(),
        &[i32::try_from(child.id().unwrap()).unwrap()],
    );
    let _ = child.kill().await;
    for pid in below {
        kill(pid);
    }
}
