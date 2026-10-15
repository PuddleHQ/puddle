// SPDX-License-Identifier: GPL-3.0-or-later
//! The release-feature check: no test double ships. Puddle's crates keep their in-memory doubles
//! (the test resolver, the allow-any address check, the fuzz entry points) behind a `testing` or
//! `fuzzing` feature that only dev-dependencies turn on. A feature that leaked into a normal
//! dependency edge would put an allow-any address check into the proxy a user runs, so the
//! shipped programs' dependency trees are resolved the way a release build resolves them (normal
//! and build edges, no dev-dependencies) and searched for the features by name.

use std::path::Path;

use crate::error::{Result, XtaskError};
use crate::tools::Cargo;

/// The programs a release ships: the command line, the desktop shell and the guest agent.
pub const SHIPPED_PROGRAMS: [&str; 3] = ["puddle", "puddle-app", "puddle-agent"];

/// Feature names that exist for tests and tools only.
pub const TEST_ONLY_FEATURES: [&str; 2] = ["testing", "fuzzing"];

/// A test-only feature found enabled in a shipped tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Leak {
    /// The package that has the feature on, as `cargo tree` prints it (`name vVERSION (path)`).
    pub package: String,
    /// The feature.
    pub feature: String,
}

impl std::fmt::Display for Leak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} has the `{}` feature on", self.package, self.feature)
    }
}

/// The test-only features on in `cargo tree --prefix none --format '{p}|{f}'` output.
///
/// # Errors
///
/// [`XtaskError::Parse`] for a line without the `|` separator.
pub fn leaks(tree_output: &str) -> Result<Vec<Leak>> {
    let mut found = Vec::new();
    for line in tree_output.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let (package, features) = line.split_once('|').ok_or_else(|| {
            XtaskError::parse("cargo tree output", format!("no `|` in line {line:?}"))
        })?;
        // `cargo tree` appends ` (*)` to a package it has printed before.
        let features = features.split_whitespace().next().unwrap_or_default();
        for feature in features.split(',') {
            if TEST_ONLY_FEATURES.contains(&feature) {
                found.push(Leak {
                    package: package.to_owned(),
                    feature: feature.to_owned(),
                });
            }
        }
    }
    found.sort_by(|a, b| (&a.package, &a.feature).cmp(&(&b.package, &b.feature)));
    found.dedup();
    Ok(found)
}

/// Checks every shipped program on every target, with default features and with all of them.
///
/// # Errors
///
/// [`XtaskError::Tool`] when cargo fails, [`XtaskError::Parse`] on odd output, and
/// [`XtaskError::Leak`] naming each leak when a test-only feature is on in a shipped tree.
pub fn check(cargo: &Cargo, manifest: &Path, targets: &[&str]) -> Result<String> {
    let mut problems = Vec::new();
    for target in targets {
        for all_features in [false, true] {
            let tree = cargo.feature_tree(
                manifest,
                &SHIPPED_PROGRAMS,
                "normal,build",
                target,
                all_features,
            )?;
            problems.extend(describe(&tree, target, all_features)?);
        }
    }
    verdict(problems, targets.len())
}

/// The leaks in one `cargo tree` output, each with where it was found.
fn describe(tree: &str, target: &str, all_features: bool) -> Result<Vec<String>> {
    let features = if all_features {
        "all features"
    } else {
        "default features"
    };
    Ok(leaks(tree)?
        .into_iter()
        .map(|leak| format!("{leak} (target {target}, {features})"))
        .collect())
}

/// The command's answer: the leaks as an error, else the line that says what was checked.
fn verdict(mut problems: Vec<String>, targets: usize) -> Result<String> {
    if !problems.is_empty() {
        problems.sort();
        problems.dedup();
        return Err(XtaskError::Leak(problems.join("\n")));
    }
    Ok(format!(
        "release features ok: no `{}` feature is on in {} on {targets} targets",
        TEST_ONLY_FEATURES.join("` or `"),
        SHIPPED_PROGRAMS.join(", "),
    ))
}

#[cfg(test)]
mod tests {
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
        let line = crate::run(
            ["release-features", "--offline", "--target", "host-tuple"].map(OsString::from),
        )
        .unwrap();
        assert!(line.ends_with("on 1 targets"), "{line}");
        let err = crate::run(["release-features", "--tag"].map(OsString::from)).unwrap_err();
        assert!(err.to_string().contains("unknown argument --tag"), "{err}");
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
}
