// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(clippy::unwrap_used, reason = "test code: a panic is how a test fails")]
//! The fake tool leaves no coverage profile file: the tests kill it on a timeout, and a kill
//! during the exit-time write leaves a cut file that fails the whole coverage merge.

mod common;

use common::Fakes;

#[test]
fn the_fake_tool_writes_no_profile_file_on_exit() {
    let fakes = Fakes::new();
    let tool = fakes.install("gh", "[]\nexit=3\nstdout=ok\\n\n");
    let profiles = tempfile::tempdir().unwrap();
    let out = std::process::Command::new(&tool)
        .env(
            "LLVM_PROFILE_FILE",
            profiles.path().join("fake-%p-%m.profraw"),
        )
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert_eq!(out.stdout, b"ok\n");
    let left: Vec<_> = std::fs::read_dir(profiles.path()).unwrap().collect();
    assert!(left.is_empty(), "profile files left: {left:?}");
}
