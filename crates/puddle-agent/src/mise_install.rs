// SPDX-License-Identifier: GPL-3.0-or-later
//! The install of a workspace's managed tools: `puddle-agent mise-install <workspace-dir>`. The
//! host runs it as the session user after [`crate::mise`] put the pinned mise on the volume, and
//! again after a clone or when the user allowed a host the install waited on.
//!
//! It finds the folders with a mise config (the workspace folder itself and each folder right
//! below it: the checkouts), asks mise in each which tools are missing, and runs
//! `mise install --yes` in safe mode (`MISE_SAFE=1`: nothing in a repository's config runs) in
//! each folder that misses one. A folder that misses nothing gets no install, so a start with
//! every tool in place makes no connection.
//!
//! With `--missing` it only reports, which is how the host follows an install's progress.
//!
//! What it prints, one record per line, and `end` last (the host refuses a report without it):
//!
//! ```text
//! missing node@22.11.0     before the install (and the whole report with --missing)
//! installed node@22.11.0   in place after the install
//! failed python@3.13.1     still missing after the install
//! end
//! ```
//!
//! The exit code is 1 when a tool is still missing or mise could not be asked; the last line on
//! standard error then says why (mise's own last error line).

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The files that make a folder one with a mise config (idiomatic files such as `.nvmrc` stay
/// off, as mise's own default).
pub const CONFIG_FILES: [&str; 5] = [
    "mise.toml",
    ".mise.toml",
    "mise.local.toml",
    ".config/mise.toml",
    ".tool-versions",
];

/// The pinned binary below the workspace folder (where `mise-fetch` puts it).
const BINARY: &str = ".puddle/mise/bin/mise";

/// Usage text of the subcommand.
pub const USAGE: &str = "usage: puddle-agent mise-install <workspace-dir> [--missing]";

/// What the host sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The workspace folder (`/workspaces/<id>`).
    pub root: PathBuf,
    /// Only report what is missing; install nothing.
    pub report_only: bool,
}

impl Request {
    /// Reads `<workspace-dir> [--missing]`.
    ///
    /// # Errors
    ///
    /// The usage text for anything else.
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Self, String> {
        let (root, report_only) = match args {
            [root] => (root.as_ref(), false),
            [root, flag] if flag.as_ref() == "--missing" => (root.as_ref(), true),
            _ => return Err(USAGE.to_owned()),
        };
        if root.is_empty() {
            return Err(USAGE.to_owned());
        }
        Ok(Self {
            root: PathBuf::from(root),
            report_only,
        })
    }

    /// The pinned mise binary of this workspace.
    #[must_use]
    pub fn binary(&self) -> PathBuf {
        self.root.join(BINARY)
    }
}

/// Why mise could not be asked or did not install everything: its last error line.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MiseFailed(pub String);

/// The two things the install asks of mise, so the logic runs without a real mise in tests.
pub trait Mise {
    /// The tools missing for `folder`, as `<name>@<version>`.
    ///
    /// # Errors
    ///
    /// Why mise could not tell.
    fn missing(&self, folder: &Path) -> Result<BTreeSet<String>, MiseFailed>;

    /// Installs what `folder` asks for.
    ///
    /// # Errors
    ///
    /// Mise's last error line when the install failed.
    fn install(&self, folder: &Path) -> Result<(), MiseFailed>;
}

/// The real mise: the pinned binary, run in the folder.
#[derive(Debug, Clone)]
pub struct SystemMise {
    binary: PathBuf,
}

impl SystemMise {
    /// The binary at `binary`.
    #[must_use]
    pub fn new(binary: PathBuf) -> Self {
        Self { binary }
    }

    fn command(&self, folder: &Path) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .current_dir(folder)
            // Plain lines: the host quotes the last one.
            .env("NO_COLOR", "1")
            .env("MISE_COLOR", "0");
        command
    }
}

impl Mise for SystemMise {
    fn missing(&self, folder: &Path) -> Result<BTreeSet<String>, MiseFailed> {
        let output = self
            .command(folder)
            .args(["ls", "--missing", "--json"])
            .output()
            .map_err(|e| MiseFailed(format!("could not run mise: {e}")))?;
        if !output.status.success() {
            return Err(MiseFailed(last_line(&output.stderr).unwrap_or_else(|| {
                format!("mise ls ended with {}", output.status)
            })));
        }
        parse_missing(&String::from_utf8_lossy(&output.stdout))
    }

    fn install(&self, folder: &Path) -> Result<(), MiseFailed> {
        let output = self
            .command(folder)
            .args(["install", "--yes"])
            .env("MISE_SAFE", "1")
            .output()
            .map_err(|e| MiseFailed(format!("could not run mise: {e}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(MiseFailed(last_line(&output.stderr).unwrap_or_else(|| {
                format!("mise install ended with {}", output.status)
            })))
        }
    }
}

/// The last line with text in `bytes`, trimmed.
fn last_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

/// The tools in `mise ls --missing --json`: an object of tool name to a list of versions.
///
/// # Errors
///
/// When the output is not that shape.
pub fn parse_missing(json: &str) -> Result<BTreeSet<String>, MiseFailed> {
    #[derive(serde::Deserialize)]
    struct Entry {
        version: String,
    }
    let tools: std::collections::BTreeMap<String, Vec<Entry>> = serde_json::from_str(json)
        .map_err(|e| MiseFailed(format!("mise ls printed something unexpected: {e}")))?;
    Ok(tools
        .into_iter()
        .flat_map(|(name, entries)| {
            entries
                .into_iter()
                .map(move |e| format!("{name}@{}", e.version))
        })
        .collect())
}

/// The folders with a mise config: `root` first when it has one, then each folder right below it
/// that has one, by name. Hidden folders (`.puddle` among them) are skipped.
#[must_use]
pub fn config_folders(root: &Path) -> Vec<PathBuf> {
    let has_config = |dir: &Path| CONFIG_FILES.iter().any(|f| dir.join(f).is_file());
    let mut below: Vec<PathBuf> = std::fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|dir| has_config(dir))
        .collect();
    below.sort();
    let mut folders = Vec::with_capacity(below.len() + 1);
    if has_config(root) {
        folders.push(root.to_path_buf());
    }
    folders.extend(below);
    folders
}

/// What a run found and did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Missing before the install.
    pub missing: BTreeSet<String>,
    /// In place after it.
    pub installed: BTreeSet<String>,
    /// Still missing after it.
    pub failed: BTreeSet<String>,
    /// Why a tool is still missing or mise could not be asked: the last error line seen.
    pub error: Option<MiseFailed>,
}

impl Report {
    /// The lines the host reads (see the module docs).
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (word, tools) in [
            ("missing", &self.missing),
            ("installed", &self.installed),
            ("failed", &self.failed),
        ] {
            for tool in tools {
                let _ = writeln!(out, "{word} {tool}"); // writing to a String cannot fail
            }
        }
        out.push_str("end\n");
        out
    }

    /// Whether the run leaves nothing missing and asked mise without an error.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.failed.is_empty() && self.error.is_none()
    }
}

/// Asks `mise` about each of `folders` and, unless `report_only`, installs in those that miss a
/// tool. A folder whose install fails does not stop the others.
pub fn run(mise: &impl Mise, folders: &[PathBuf], report_only: bool) -> Report {
    let mut report = Report::default();
    let mut to_install = Vec::new();
    for folder in folders {
        match mise.missing(folder) {
            Ok(missing) if missing.is_empty() => {}
            Ok(missing) => {
                report.missing.extend(missing);
                to_install.push(folder);
            }
            Err(e) => report.error = Some(e),
        }
    }
    if report_only || to_install.is_empty() {
        return report;
    }
    for folder in &to_install {
        if let Err(e) = mise.install(folder) {
            report.error = Some(e);
        }
    }
    let mut still = BTreeSet::new();
    for folder in &to_install {
        match mise.missing(folder) {
            Ok(missing) => still.extend(missing),
            Err(e) => report.error = Some(e),
        }
    }
    report.installed = report.missing.difference(&still).cloned().collect();
    report.failed = report.missing.intersection(&still).cloned().collect();
    report
}

#[cfg(test)]
mod tests;
