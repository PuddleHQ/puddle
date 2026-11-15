// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `puddle-freshness` program as the scheduled workflow runs it.
#![expect(
    clippy::unwrap_used,
    reason = "a test helper fails the test by panicking"
)]

use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_puddle-freshness"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn it_signs_checks_and_prints_to_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("freshness.json");
    let file = file.to_str().unwrap();
    let key = String::from_utf8(run(&["public-key", "--placeholder-key"]).stdout).unwrap();
    let signed = run(&["refresh", "--placeholder-key", "--file", file]);
    assert!(signed.status.success());
    assert!(String::from_utf8_lossy(&signed.stdout).contains("sequence 1"));
    let checked = run(&["verify", "--key", key.trim(), "--file", file]);
    assert!(checked.status.success(), "{checked:?}");
}

#[test]
fn it_reports_an_error_on_stderr_and_fails() {
    let out = run(&["verify"]);
    assert!(!out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        String::from_utf8_lossy(&out.stderr).starts_with("puddle-freshness: verify needs --key")
    );
}
