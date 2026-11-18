// SPDX-License-Identifier: AGPL-3.0-or-later
//! `puddle-lab-forgejo`, the lab's Forgejo as a program, serves SSH too: its info file names the
//! SSH port, the key the account holds and the host key fingerprint, and the real `git` clones
//! `ssh://git@127.0.0.1:<port>/acme/app.git` with that key after checking the fingerprint
//! against what the server presents.
//!
//! Needs `PUDDLE_FORGEJO_BIN` like the fixture (skips without it unless `PUDDLE_REQUIRE_FORGEJO`
//! is set). Unix only: the client side uses `ssh-keygen`, `ssh-keyscan` and an OpenSSH client that
//! checks the key file's mode, and the Windows runner's OpenSSH differs in all three. The program
//! itself writes the same info file on Windows.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![cfg(unix)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use puddle_e2e::lab::clean_git;

/// The running program; stopped (through its stop file, then a kill) when dropped.
struct Program {
    child: Child,
    stop: std::path::PathBuf,
}

impl Drop for Program {
    fn drop(&mut self) {
        std::fs::write(&self.stop, "").ok();
        if self.child.wait().is_err() {
            self.child.kill().ok();
        }
    }
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.stdin(Stdio::null()).output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "{cmd:?}: {text}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether Forgejo is configured, or why a required one is not: any value of
/// `PUDDLE_REQUIRE_FORGEJO` makes a missing `PUDDLE_FORGEJO_BIN` fail at once, naming it.
fn forgejo_configured(set: &dyn Fn(&str) -> bool) -> Result<bool, String> {
    let configured = set("PUDDLE_FORGEJO_BIN");
    if set("PUDDLE_REQUIRE_FORGEJO") && !configured {
        return Err("PUDDLE_REQUIRE_FORGEJO is set but PUDDLE_FORGEJO_BIN is not".to_owned());
    }
    Ok(configured)
}

#[test]
fn a_required_forgejo_that_is_not_configured_fails_naming_the_variable() {
    let err = forgejo_configured(&|name| name == "PUDDLE_REQUIRE_FORGEJO").unwrap_err();
    assert!(err.contains("PUDDLE_FORGEJO_BIN"), "{err}");
}

#[test]
fn forgejo_not_required_and_not_configured_skips_and_configured_runs() {
    assert_eq!(forgejo_configured(&|_| false), Ok(false));
    assert_eq!(
        forgejo_configured(&|name| name == "PUDDLE_FORGEJO_BIN"),
        Ok(true)
    );
}

#[test]
fn the_program_serves_ssh_and_its_info_file_lets_git_clone_with_the_key_and_check_the_host_key() {
    let set = |name: &str| std::env::var_os(name).is_some();
    // skip-ok: the program exits at once without PUDDLE_FORGEJO_BIN, and the lab workflow sets
    // PUDDLE_REQUIRE_FORGEJO so a missing Forgejo fails there.
    if !forgejo_configured(&set).unwrap() {
        eprintln!("skipped: set PUDDLE_FORGEJO_BIN");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let info_file = dir.path().join("info.json");
    let stop = dir.path().join("stop");
    let port = puddle_test_kit::quiet_port();
    let child = Command::new(env!("CARGO_BIN_EXE_puddle-lab-forgejo"))
        .args(["--name", "git.lab.test", "--port", &port.to_string()])
        .arg("--info")
        .arg(&info_file)
        .arg("--stop")
        .arg(&stop)
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let mut program = Program { child, stop };
    let deadline = Instant::now() + Duration::from_mins(3);
    // flake-ok: waiting for a program to write its file, bounded, failing with its state.
    let info: serde_json::Value = loop {
        if let Ok(text) = std::fs::read_to_string(&info_file) {
            break serde_json::from_str(&text).unwrap();
        }
        assert!(
            program.child.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "the program wrote no info file"
        );
        std::thread::sleep(Duration::from_millis(250));
    };

    let ssh_port = info["ssh_port"].as_u64().unwrap();
    assert_ne!(ssh_port, u64::from(port), "SSH has a port of its own");
    let key = Path::new(info["ssh_key"].as_str().unwrap());
    let fingerprint = info["ssh_host_key_fingerprint"].as_str().unwrap();
    assert!(fingerprint.starts_with("SHA256:"), "{fingerprint}");
    assert!(
        info["ssh_host_key_fingerprints"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!(fingerprint))
    );

    // The fingerprint in the info file is one the server really presents.
    let known = dir.path().join("known_hosts");
    let scanned = run(Command::new("ssh-keyscan").args(["-p", &ssh_port.to_string(), "127.0.0.1"]));
    std::fs::write(&known, &scanned).unwrap();
    let shown = run(Command::new("ssh-keygen").arg("-lf").arg(&known));
    assert!(
        shown.contains(fingerprint),
        "info file says {fingerprint}, the server presents:\n{shown}"
    );

    let ssh = format!(
        "ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile={}",
        key.display(),
        known.display()
    );
    let clone = dir.path().join("app");
    run(clean_git(dir.path())
        .env("GIT_SSH_COMMAND", &ssh)
        .args([
            "clone",
            "-q",
            &format!("ssh://git@127.0.0.1:{ssh_port}/acme/app.git"),
        ])
        .arg(&clone));
    assert!(
        clone.join("package.json").is_file(),
        "the clone has the repository's files"
    );
}
