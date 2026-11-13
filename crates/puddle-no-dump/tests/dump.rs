// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the kernel does with a process that has called `harden_from_env`, in a child process
//! (the setting is per process). The child is this test binary run again on `probe`.
#![cfg(target_os = "linux")]
#![expect(clippy::expect_used, reason = "a test fails by panicking")]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

use puddle_no_dump::{ALLOW_VAR, Outcome, harden_from_env};

const MODE_VAR: &str = "PUDDLE_NO_DUMP_PROBE";

/// The child's half. In the parent's run (no mode) it does nothing and passes.
#[test]
fn probe() {
    let Ok(mode) = std::env::var(MODE_VAR) else {
        // skip-ok: a helper role, run as a child with the mode set by the tests below
        return;
    };
    let outcome = harden_from_env();
    println!("ready {outcome}");
    if mode == "abort" {
        std::process::abort();
    }
    // `wait`: stay alive until the parent closes stdin.
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

fn child(mode: &str, allow: bool, dir: &Path) -> Command {
    // A core limit of unlimited, so only the process's own setting decides; `exec` keeps the pid.
    let mut cmd = Command::new("sh");
    cmd.args([
        "-c",
        r#"ulimit -c unlimited && exec "$0" probe --exact --nocapture --test-threads=1"#,
    ])
    .arg(std::env::current_exe().expect("test binary path"))
    .current_dir(dir)
    .env(MODE_VAR, mode)
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    if allow {
        cmd.env(ALLOW_VAR, "1");
    } else {
        cmd.env_remove(ALLOW_VAR);
    }
    cmd
}

/// Starts a child that waits, and says whether this process may open its memory.
fn can_open_proc_mem_of_child(allow: bool) -> (bool, String) {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut child = child("wait", allow, dir.path())
        .spawn()
        .expect("start the child");
    let mut out = BufReader::new(child.stdout.take().expect("child stdout"));
    let ready = loop {
        let mut line = String::new();
        assert!(
            out.read_line(&mut line).expect("read child") > 0,
            "child ended before it was ready"
        );
        // libtest prints `test probe ... ` in front of what the test prints.
        if let Some((_, rest)) = line.trim().split_once("ready ") {
            break rest.to_owned();
        }
    };
    let open = std::fs::File::open(format!("/proc/{}/mem", child.id())).is_ok();
    drop(child.stdin.take());
    child.wait().expect("child ends");
    (open, ready)
}

#[test]
fn a_hardened_process_cannot_be_read_through_proc_mem_by_its_own_user() {
    let (open, ready) = can_open_proc_mem_of_child(false);
    assert_eq!(ready, Outcome::Protected.to_string());
    assert!(
        !open,
        "/proc/<pid>/mem of a non-dumpable process opened for the same user"
    );
}

#[test]
fn the_opt_out_leaves_the_process_readable_so_the_test_above_proves_something() {
    let (open, ready) = can_open_proc_mem_of_child(true);
    assert_eq!(ready, Outcome::Allowed.to_string());
    assert!(
        open,
        "/proc/<pid>/mem of a dumpable child did not open: the ptrace rules of this system block the check"
    );
}

/// Runs a child that aborts. Returns what it said before it did and the files it left in its folder.
fn abort_run(allow: bool) -> (String, Vec<String>) {
    let dir = tempfile::tempdir().expect("temp dir");
    let output = child("abort", allow, dir.path())
        .output()
        .expect("run the child");
    assert!(!output.status.success(), "the child was meant to abort");
    let said = String::from_utf8_lossy(&output.stdout).into_owned();
    let files = std::fs::read_dir(dir.path())
        .expect("list the folder")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    (said, files)
}

/// Whether a core file lands in the working folder under this system's `core_pattern`: a plain
/// relative name does; a pipe (apport, systemd-coredump) or an absolute folder does not.
fn cores_land_in_the_working_folder() -> bool {
    let pattern = std::fs::read_to_string("/proc/sys/kernel/core_pattern").unwrap_or_default();
    let pattern = pattern.trim();
    !pattern.is_empty() && !pattern.starts_with('|') && !pattern.starts_with('/')
}

#[test]
fn a_crash_leaves_no_core_file_and_with_the_opt_out_it_does() {
    let (said, files) = abort_run(false);
    assert!(said.contains("ready protected"), "{said}");
    let (said_open, files_open) = abort_run(true);
    assert!(said_open.contains("ready allowed"), "{said_open}");
    // A system that pipes cores to a collector (apport, systemd-coredump) leaves nothing to see in
    // the folder either way; there, the two `/proc/<pid>/mem` tests above carry the proof, and the
    // folder check runs on a machine with a plain `core_pattern` (WSL, most containers).
    if cores_land_in_the_working_folder() {
        assert!(
            files_open.iter().any(|f| f.starts_with("core")),
            "no core with the opt-out, so the check cannot see cores: {files_open:?}"
        );
        assert_eq!(files, Vec::<String>::new());
    }
}
