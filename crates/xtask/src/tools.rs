// SPDX-License-Identifier: GPL-3.0-or-later
//! Running the external tools xtask drives: cargo (tree, cargo-about) and git.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::error::{Result, XtaskError};
use crate::inventory::{Inventory, Package, shipped_packages};

/// The variables `git rev-parse --local-env-vars` lists: the ones that make git act on one
/// particular repository. A git hook (pre-commit, pre-push) exports them, and a `git` started with
/// them acts on the hook's repository whatever its working directory is.
const GIT_LOCAL_ENV: [&str; 15] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// `git` without the repository variables of the caller's environment (the `GIT_LOCAL_ENV` list), so
/// it acts on the repository its working directory or `-C` names, also when xtask runs from a hook.
#[must_use]
pub fn git_command() -> Command {
    let mut cmd = Command::new("git");
    for var in GIT_LOCAL_ENV {
        cmd.env_remove(var);
    }
    cmd
}

/// Runs `command` and returns its stdout.
///
/// # Errors
///
/// [`XtaskError::Tool`] when it can't start or exits non-zero (with the end of its stderr).
pub fn run(command: &mut Command) -> Result<String> {
    let shown = format!(
        "{} {}",
        command.get_program().to_string_lossy(),
        command
            .get_args()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let out = command.output().map_err(|e| XtaskError::Tool {
        command: shown.clone(),
        detail: if e.kind() == std::io::ErrorKind::NotFound {
            let program = command.get_program().to_string_lossy();
            format!(
                "cannot start: {e}; `{program}` is not installed or not on PATH, install it and run the task again"
            )
        } else {
            format!("cannot start: {e}")
        },
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = {
            let chars: Vec<char> = stderr.trim_end().chars().collect();
            chars
                .iter()
                .skip(chars.len().saturating_sub(2000))
                .collect()
        };
        return Err(XtaskError::Tool {
            command: shown,
            detail: format!("{}\n{tail}", out.status),
        });
    }
    String::from_utf8(out.stdout).map_err(|e| XtaskError::parse(format!("output of `{shown}`"), e))
}

/// Which features a cargo tree is resolved with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Features {
    /// `--all-features`.
    All,
    /// `--no-default-features --features <list>`.
    Only(Vec<String>),
}

impl Features {
    fn args(&self) -> Vec<String> {
        match self {
            Self::All => vec!["--all-features".into()],
            Self::Only(list) => vec![
                "--no-default-features".into(),
                "--features".into(),
                list.join(" "),
            ],
        }
    }
}

/// The cargo to run: `$CARGO` (set when xtask runs under `cargo run`) or `cargo`.
#[derive(Debug, Clone)]
pub struct Cargo {
    program: OsString,
    offline: bool,
}

impl Cargo {
    /// The cargo from the environment; `offline` adds `--offline` to every call.
    #[must_use]
    pub fn from_env(offline: bool) -> Self {
        Self {
            program: std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()),
            offline,
        }
    }

    fn command(&self) -> Command {
        Command::new(&self.program)
    }

    fn common(&self, cmd: &mut Command, manifest: &Path, features: &Features) {
        cmd.arg("--manifest-path")
            .arg(manifest)
            .args(features.args())
            .arg("--locked");
        if self.offline {
            cmd.arg("--offline");
        }
    }

    /// The packages the tree at `manifest` ships on any of `targets` ([`shipped_packages`]):
    /// the whole workspace when `workspace`, else the manifest's package.
    ///
    /// # Errors
    ///
    /// [`XtaskError::Tool`] or [`XtaskError::Parse`].
    pub fn shipped(
        &self,
        manifest: &Path,
        features: &Features,
        targets: &[&str],
        workspace: bool,
    ) -> Result<BTreeSet<Package>> {
        let mut all = BTreeSet::new();
        for target in targets {
            let mut cmd = self.command();
            cmd.args([
                "tree", "--edges", "normal", "--prefix", "none", "--format", "{p}", "--target",
                target,
            ]);
            if workspace {
                cmd.arg("--workspace");
            }
            self.common(&mut cmd, manifest, features);
            all.extend(shipped_packages(&run(&mut cmd)?)?);
        }
        Ok(all)
    }

    /// `cargo tree` of `packages` on `target` with the given dependency `edges` (`normal,build`
    /// is what a release build links), default features or all of them, printed as
    /// `package|features` for [`crate::features::leaks`].
    ///
    /// # Errors
    ///
    /// [`XtaskError::Tool`] when cargo fails.
    pub fn feature_tree(
        &self,
        manifest: &Path,
        packages: &[&str],
        edges: &str,
        target: &str,
        all_features: bool,
    ) -> Result<String> {
        let mut cmd = self.command();
        cmd.args([
            "tree", "--edges", edges, "--prefix", "none", "--format", "{p}|{f}",
        ])
        .args(["--target", target, "--locked"])
        .arg("--manifest-path")
        .arg(manifest);
        for package in packages {
            cmd.args(["--package", package]);
        }
        if all_features {
            cmd.arg("--all-features");
        }
        if self.offline {
            cmd.arg("--offline");
        }
        run(&mut cmd)
    }

    /// cargo-about's JSON report for the tree at `manifest` on `targets`, with the config at
    /// `config`. The targets go on the command line (they override the config's `targets`), so
    /// the report covers the same platforms as [`Cargo::shipped`] and the config cannot drift.
    ///
    /// # Errors
    ///
    /// [`XtaskError::Tool`] (cargo-about missing: `cargo install cargo-about --locked --features cli`).
    pub fn about(
        &self,
        manifest: &Path,
        config: &Path,
        features: &Features,
        targets: &[&str],
    ) -> Result<String> {
        let mut cmd = self.command();
        cmd.args(["about", "generate", "--format", "json", "--config"])
            .arg(config);
        for target in targets {
            cmd.args(["--target", target]);
        }
        self.common(&mut cmd, manifest, features);
        run(&mut cmd)
    }

    /// The checked inventory of a tree: cargo-about's report plus the shipped packages.
    ///
    /// # Errors
    ///
    /// As [`Cargo::shipped`] and [`Cargo::about`].
    pub fn inventory(
        &self,
        tree: &str,
        manifest: &Path,
        config: &Path,
        features: &Features,
        targets: &[&str],
        workspace: bool,
    ) -> Result<Inventory> {
        let required = self.shipped(manifest, features, targets, workspace)?;
        Inventory::new(
            tree,
            &self.about(manifest, config, features, targets)?,
            required,
        )
    }
}

#[cfg(test)]
mod tests;
