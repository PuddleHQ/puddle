// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests for the extension-host cleanup with real processes: shell scripts below a fake server
//! folder stand in for the server's extension host, its pty host and the processes below them.
#![cfg(target_os = "linux")]
#![expect(
    clippy::unwrap_used,
    clippy::panic,
    reason = "helpers outside #[test] fns run only in tests"
)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use puddle_agent::exthost::{self, Proc};
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
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let tree_size = exthost::extension_hosts(&server.procs(), &server.folders()).len();
        let sleepers = exthost::snapshot()
            .iter()
            .filter(|p| p.argv.first().is_some_and(|a| a == "sleep") && p.ppid != 1)
            .count();
        if tree_size >= 1 && sleepers >= 2 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the fake processes did not start");
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
    let deadline = Instant::now() + Duration::from_secs(20);
    while exthost::extension_hosts(&server.procs(), &server.folders()).len() < 20
        || server.stubborn().len() < 20
    {
        assert!(Instant::now() < deadline, "the fake hosts did not start");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
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

#[tokio::test]
async fn an_orphaned_debuggee_with_a_dead_socket_is_swept_and_one_with_a_live_socket_is_not() {
    // Orphans go to the nearest ancestor that is a subreaper, not to process 1: be that ancestor.
    rustix::process::set_child_subreaper(Some(rustix::process::getpid())).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("live.sock");
    std::fs::write(&live, "").unwrap();
    let options = |sock: &Path| format!("{{\"inspectorIpc\":\"{}\"}}", sock.display());
    // Double fork so the shell's parent exits at once and the debuggee is reparented.
    let start = |sock: PathBuf, tag: &str| {
        let script = format!(
            "(sleep 601 & echo $! >{}/{tag}.pid) &",
            dir.path().display()
        );
        std::process::Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("VSCODE_INSPECTOR_OPTIONS", options(&sock))
            .status()
            .unwrap();
    };
    start(dir.path().join("dead.sock"), "dead");
    start(live.clone(), "live");
    let read = |tag: &str| -> i32 {
        loop {
            if let Ok(text) = std::fs::read_to_string(dir.path().join(format!("{tag}.pid")))
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    let (dead, live_pid) = (read("dead"), read("live"));
    let me = i32::try_from(std::process::id()).unwrap();
    // Wait until both orphans have been adopted by this process, the reaper the test names.
    let deadline = Instant::now() + Duration::from_secs(10);
    while ![dead, live_pid].iter().all(|pid| {
        exthost::snapshot()
            .iter()
            .any(|p| p.pid == *pid && p.ppid == me)
    }) {
        assert!(Instant::now() < deadline, "the orphans were not adopted");
        std::thread::sleep(Duration::from_millis(20));
    }

    let swept = exthost::sweep_orphans_of(me).await;

    let running = |pid: i32| {
        exthost::snapshot()
            .iter()
            .any(|p| p.pid == pid && !p.zombie)
    };
    let (dead_left, live_left) = (running(dead), running(live_pid));
    kill(live_pid);
    assert_eq!(swept, 1);
    assert!(!dead_left, "the orphan with a dead socket survived");
    assert!(live_left, "the debuggee with a live socket was ended");
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
