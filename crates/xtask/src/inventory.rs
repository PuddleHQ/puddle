// SPDX-License-Identifier: AGPL-3.0-or-later
//! Third-party notices: cargo-about's licence report, checked against the dependency graph Cargo
//! resolves, and rendered as a plain-text notices file.
//!
//! cargo-about only warns (and carries on) when a crate has no licence it can attribute, so the
//! notices could silently miss a dependency. [`Inventory::check`] closes that gap: every package
//! the shipped binaries link (normal dependencies, incl. proc-macros, on the shipped targets, minus
//! our own crates) must appear in some licence's "used by" list. Build-script dependencies run on
//! the build machine and aren't in the binaries; cargo-about may still list them (harmless).

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::Deserialize;

use crate::error::{Result, XtaskError};

/// A package by name and version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Package {
    /// Crate name.
    pub name: String,
    /// Exact version.
    pub version: String,
}

impl std::fmt::Display for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.name, self.version)
    }
}

/// One licence text and the crates attributed under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenceGroup {
    /// SPDX id.
    pub id: String,
    /// Full licence name.
    pub name: String,
    /// The licence text as the crates ship it (with their copyright lines).
    pub text: String,
    /// The crates, with their repository URL when known (MPL-2.0 needs a pointer to the source).
    pub used_by: Vec<(Package, Option<String>)>,
}

/// A dependency tree's licences and the packages that must be covered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inventory {
    tree: String,
    groups: Vec<LicenceGroup>,
    required: BTreeSet<Package>,
}

#[derive(Deserialize)]
struct AboutJson {
    licenses: Vec<AboutLicence>,
}

#[derive(Deserialize)]
struct AboutLicence {
    name: String,
    id: String,
    text: String,
    used_by: Vec<AboutUse>,
}

#[derive(Deserialize)]
struct AboutUse {
    #[serde(rename = "crate")]
    krate: AboutCrate,
}

#[derive(Deserialize)]
struct AboutCrate {
    name: String,
    version: String,
    repository: Option<String>,
}

/// The packages a tree ships, from `cargo tree -e normal --prefix none -f {p}` output: every
/// registry or git package on a line, without local path packages (our own crates; for msb its
/// workspace crates, whose licence is in NOTICE). `cargo tree` resolves features the way a real
/// build does, so features only dev-dependencies turn on don't add packages.
///
/// # Errors
///
/// [`XtaskError::Parse`] for a line that isn't `name vVERSION[ (...)]`.
pub fn shipped_packages(tree_output: &str) -> Result<BTreeSet<Package>> {
    let mut packages = BTreeSet::new();
    for line in tree_output.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let bad = || XtaskError::parse("cargo tree output", format!("unexpected line {line:?}"));
        let mut parts = line.splitn(3, ' ');
        let (Some(name), Some(version)) = (parts.next(), parts.next()) else {
            return Err(bad());
        };
        let version = version
            .strip_prefix('v')
            .filter(|v| !v.is_empty())
            .ok_or_else(bad)?;
        let rest = parts.next().unwrap_or_default();
        if is_local(rest) {
            continue;
        }
        packages.insert(Package {
            name: name.to_owned(),
            version: version.to_owned(),
        });
    }
    Ok(packages)
}

/// Whether the annotations after the version (`(proc-macro)`, `(*)`, a source) name a local
/// path: `(/...)` or `(C:\\...)`.
fn is_local(annotations: &str) -> bool {
    annotations
        .split('(')
        .skip(1)
        .any(|group| group.starts_with('/') || group.get(1..3) == Some(":\\"))
}

impl Inventory {
    /// Builds the inventory of `tree` (a name for messages and the notices title) from
    /// cargo-about's JSON report and the packages that must be covered.
    ///
    /// # Errors
    ///
    /// [`XtaskError::Parse`] when `about_json` isn't a cargo-about report.
    pub fn new(tree: &str, about_json: &str, required: BTreeSet<Package>) -> Result<Self> {
        let about: AboutJson = serde_json::from_str(about_json)
            .map_err(|e| XtaskError::parse(format!("cargo-about report for {tree}"), e))?;
        let groups = about
            .licenses
            .into_iter()
            .map(|l| {
                let mut used_by: Vec<_> = l
                    .used_by
                    .into_iter()
                    .map(|u| {
                        let package = Package {
                            name: u.krate.name,
                            version: u.krate.version,
                        };
                        (package, u.krate.repository)
                    })
                    .collect();
                used_by.sort();
                used_by.dedup();
                LicenceGroup {
                    id: l.id,
                    name: l.name,
                    text: l.text,
                    used_by,
                }
            })
            .collect();
        Ok(Self {
            tree: tree.to_owned(),
            groups,
            required,
        })
    }

    /// The licence groups, as cargo-about reported them.
    #[must_use]
    pub fn groups(&self) -> &[LicenceGroup] {
        &self.groups
    }

    /// Required packages that no licence group mentions.
    #[must_use]
    pub fn missing(&self) -> Vec<&Package> {
        let covered: BTreeSet<&Package> = self
            .groups
            .iter()
            .flat_map(|g| g.used_by.iter().map(|(p, _)| p))
            .collect();
        self.required
            .iter()
            .filter(|p| !covered.contains(p))
            .collect()
    }

    /// Fails when a required package has no licence entry.
    ///
    /// # Errors
    ///
    /// [`XtaskError::MissingLicence`] naming every such package.
    pub fn check(&self) -> Result<()> {
        let missing = self.missing();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(XtaskError::MissingLicence {
                tree: self.tree.clone(),
                packages: missing.iter().map(ToString::to_string).collect(),
            })
        }
    }

    /// The notices file: an intro, then per licence text the crates that use it and the text.
    #[must_use]
    pub fn render(&self, intro: &str) -> String {
        let rule = "=".repeat(78);
        let mut out = format!(
            "Third-party notices: {}\n\n{intro}\n\n{} packages, {} licence texts.\n",
            self.tree,
            self.required.len(),
            self.groups.len()
        );
        for g in &self.groups {
            // writing into a `String` cannot fail
            let _ = write!(out, "\n{rule}\n{} ({})\n\nUsed by:\n", g.name, g.id);
            for (p, repo) in &g.used_by {
                match repo {
                    Some(r) => {
                        // writing into a `String` cannot fail
                        let _ = writeln!(out, "  - {p} <{r}>");
                    }
                    None => {
                        // writing into a `String` cannot fail
                        let _ = writeln!(out, "  - {p}");
                    }
                }
            }
            // writing into a `String` cannot fail
            let _ = write!(out, "\n{}\n", g.text.trim_end());
        }
        out
    }
}

#[cfg(test)]
mod tests;
