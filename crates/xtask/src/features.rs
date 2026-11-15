// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
