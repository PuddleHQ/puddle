// SPDX-License-Identifier: AGPL-3.0-or-later
use std::ffi::OsString;

use super::*;

fn manifest() -> std::path::PathBuf {
    crate::workspace_root().join("Cargo.toml")
}

#[test]
fn leaks_reads_the_features_of_every_line() {
    let tree = "\
app v0.0.0 (/w/crates/app)|embedded-ui
puddle-proxy v0.0.0 (/w/crates/puddle-proxy)|testing,fuzzing
puddle-proxy v0.0.0 (/w/crates/puddle-proxy)|testing (*)
serde v1.0.0|default,derive,std
puddle-testing-tools v0.0.0 (/w/crates/x)|
";
    let found = leaks(tree).unwrap();
    let shown: Vec<String> = found.iter().map(ToString::to_string).collect();
    assert_eq!(
        shown,
        [
            "puddle-proxy v0.0.0 (/w/crates/puddle-proxy) has the `fuzzing` feature on",
            "puddle-proxy v0.0.0 (/w/crates/puddle-proxy) has the `testing` feature on",
        ],
        "a feature named like a test feature but longer, and a package named testing-something, are not leaks"
    );
    assert_eq!(leaks("").unwrap(), []);
    let err = leaks("app v0.0.0").unwrap_err().to_string();
    assert!(err.contains("no `|`"), "{err}");
}

/// The control: the same parser finds the feature where it is on. A dev-dependency edge turns
/// `testing` on for the agent's tests, so a tree that includes dev edges has leaks, and the
/// check passing on the release edges means something.
#[test]
fn the_check_sees_a_feature_that_dev_dependencies_turn_on() {
    let cargo = Cargo::from_env(true);
    let with_dev = cargo
        .feature_tree(
            &manifest(),
            &["puddle-agent"],
            "normal,build,dev",
            "host-tuple",
            false,
        )
        .unwrap();
    let found = leaks(&with_dev).unwrap();
    assert!(
        found
            .iter()
            .any(|l| l.feature == "testing" && l.package.starts_with("puddle-proxy ")),
        "{found:?}"
    );
    let release = cargo
        .feature_tree(
            &manifest(),
            &["puddle-agent"],
            "normal,build",
            "host-tuple",
            false,
        )
        .unwrap();
    assert_eq!(leaks(&release).unwrap(), []);
}

#[test]
fn no_shipped_program_has_a_test_only_feature_on() {
    let line = check(&Cargo::from_env(true), &manifest(), &["host-tuple"]).unwrap();
    assert!(line.starts_with("release features ok"), "{line}");
}

#[test]
fn the_command_checks_the_workspace_and_rejects_unknown_arguments() {
    let line =
        crate::run(["release-features", "--offline", "--target", "host-tuple"].map(OsString::from))
            .unwrap();
    assert!(line.ends_with("on 1 targets"), "{line}");
    let err = crate::run(["release-features", "--tag"].map(OsString::from)).unwrap_err();
    assert!(err.to_string().contains("unknown argument --tag"), "{err}");
}

#[test]
fn a_cargo_that_cannot_read_the_workspace_is_an_error_not_a_pass() {
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    let missing = Path::new("/nonexistent/Cargo.toml");
    let err = check(&Cargo::from_env(true), missing, &["host-tuple"]).unwrap_err();
    assert!(err.to_string().contains("failed"), "{err}");
}

#[test]
fn a_leak_is_described_with_its_target_and_feature_set() {
    let tree = "a v1.0.0 (/w/a)|testing\nb v1.0.0|default\n";
    assert_eq!(
        describe(tree, "t", true).unwrap(),
        ["a v1.0.0 (/w/a) has the `testing` feature on (target t, all features)"]
    );
    assert_eq!(
        describe(tree, "t", false).unwrap(),
        ["a v1.0.0 (/w/a) has the `testing` feature on (target t, default features)"]
    );
}

#[test]
fn leaks_become_one_error_that_names_each_once() {
    let leak = "puddle-proxy v0.0.0 has the `testing` feature on (target t, default features)";
    let err = verdict(vec![leak.into(), leak.into()], 1)
        .unwrap_err()
        .to_string();
    assert_eq!(err, format!("a test-only feature would ship:\n{leak}"));
    assert!(verdict(vec![], 3).unwrap().ends_with("on 3 targets"));
}
