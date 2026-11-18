// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Tools card's questions and edits: `puddle-agent mise-tools <workspace-dir> <operation>`.
//! The host runs it as the session user with the pinned mise on the volume.
//!
//! ```text
//! list                              the tools of every mise file in the workspace
//! set <name> <version> <hash>       tools.<name> = <version> in the root mise.toml
//! remove <name> <hash>              takes <name> out of the root mise.toml
//! registry                          the tools mise knows by name
//! versions <name>                   the newest versions mise lists for one tool
//! ```
//!
//! Edits go through mise's own commands (`mise config set`, `mise unuse --no-prune`), so every
//! other line of the file, comments included, stays as it was, and only the root file
//! (`<workspace-dir>/mise.toml`) is ever written. `<hash>` is the SHA-256 the last `list` printed
//! for that file (or `absent`); an edit refuses with exit code 5 when the file no longer has it.
//! mise edits a copy of the file, which replaces it only if the file still has that hash after mise
//! is done, so a save from the editor in between is kept and the edit refused.
//!
//! What it prints, one record per line, tab-separated, and `end` last (the host refuses output
//! without it):
//!
//! ```text
//! hash <sha256 of the root mise.toml | absent>      list, first
//! tool <file> <name> <requested> <1 if installed>   list; <file> is relative to the workspace
//! hash <new hash>                                   set and remove: what the file has now
//! reg <name> <description>                          registry
//! ver <version>                                     versions, newest first
//! end
//! ```
//!
//! The exit code is 1 when mise failed (its last error line is the last line on standard error),
//! 5 for a changed file and 2 for a bad command line.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use puddle_types::{ToolName, ToolVersion, ToolsFileHash};

use crate::download::sha256_of;
use crate::mise_install::{CONFIG_FILES, MiseFailed, config_folders, last_line};

/// Usage text of the subcommand.
pub const USAGE: &str = "usage: puddle-agent mise-tools <workspace-dir> list | set <name> <version> <hash> | remove <name> <hash> | registry | versions <name>";

/// The exit code for a root file that changed since the user looked.
pub const CONFLICT_EXIT: u8 = 5;

/// Most `versions` records printed, newest first.
pub const MAX_VERSIONS: usize = 200;

/// Most `reg` records printed.
const MAX_REGISTRY: usize = 5000;

/// Longest registry description kept, in characters.
const MAX_DESCRIPTION: usize = 200;

/// The pinned binary below the workspace folder (where `mise-fetch` puts it).
const BINARY: &str = ".puddle/mise/bin/mise";

/// What to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operation {
    /// Print the tools of every mise file.
    List,
    /// Set a tool's version in the root file.
    Set(ToolName, ToolVersion, ToolsFileHash),
    /// Take a tool out of the root file.
    Remove(ToolName, ToolsFileHash),
    /// Print the registry.
    Registry,
    /// Print the versions of one tool.
    Versions(ToolName),
}

/// What the host sends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The workspace folder (`/workspaces/<id>`).
    pub root: PathBuf,
    /// What to do there.
    pub operation: Operation,
}

impl Request {
    /// Reads `<workspace-dir> <operation> ...`.
    ///
    /// # Errors
    ///
    /// The usage text for anything else, with the refused value named.
    pub fn parse<S: AsRef<str>>(args: &[S]) -> Result<Self, String> {
        let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
        let refuse = |why: String| format!("{why}\n{USAGE}");
        let bad = |e: puddle_types::ValidationError| refuse(e.to_string());
        let [root, op, rest @ ..] = args.as_slice() else {
            return Err(USAGE.to_owned());
        };
        if root.is_empty() {
            return Err(USAGE.to_owned());
        }
        let operation = match (*op, rest) {
            ("list", []) => Operation::List,
            ("set", [name, version, hash]) => Operation::Set(
                ToolName::new(name).map_err(bad)?,
                ToolVersion::new(version).map_err(bad)?,
                ToolsFileHash::new(hash).map_err(bad)?,
            ),
            ("remove", [name, hash]) => Operation::Remove(
                ToolName::new(name).map_err(bad)?,
                ToolsFileHash::new(hash).map_err(bad)?,
            ),
            ("registry", []) => Operation::Registry,
            ("versions", [name]) => Operation::Versions(ToolName::new(name).map_err(bad)?),
            _ => return Err(USAGE.to_owned()),
        };
        Ok(Self {
            root: PathBuf::from(root),
            operation,
        })
    }

    /// The pinned mise binary of this workspace.
    #[must_use]
    pub fn binary(&self) -> PathBuf {
        self.root.join(BINARY)
    }
}

/// Why an operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Failure {
    /// The root file is not the one the user looked at.
    #[error("mise.toml changed since it was listed")]
    Changed,
    /// mise, or the file system, said no: the reason, one line.
    #[error("{0}")]
    Mise(#[from] MiseFailed),
}

/// What mise is asked, so the logic runs without a real mise in tests. Every call runs in `dir`.
pub trait Mise {
    /// Runs mise with `args` and returns its standard output.
    ///
    /// # Errors
    ///
    /// Mise's last error line when it failed or could not run.
    fn run(&self, dir: &Path, args: &[&str]) -> Result<String, MiseFailed>;
}

/// The real mise: the pinned binary, in safe mode (nothing in a repository's config runs).
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
}

impl Mise for SystemMise {
    fn run(&self, dir: &Path, args: &[&str]) -> Result<String, MiseFailed> {
        let output = std::process::Command::new(&self.binary)
            .current_dir(dir)
            .args(args)
            .env("NO_COLOR", "1")
            .env("MISE_COLOR", "0")
            .env("MISE_SAFE", "1")
            .output()
            .map_err(|e| MiseFailed(format!("could not run mise: {e}")))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Err(MiseFailed(last_line(&output.stderr).unwrap_or_else(|| {
            format!(
                "mise {} ended with {}",
                args.first().unwrap_or(&""),
                output.status
            )
        })))
    }
}

/// The records to print, or why not.
pub type Outcome = Result<String, Failure>;

/// Runs `request` with `mise`.
///
/// # Errors
///
/// [`Failure::Changed`] when an edit's hash is not the root file's, [`Failure::Mise`] when mise
/// or the file system failed.
pub fn run(mise: &impl Mise, request: &Request) -> Outcome {
    let root = request.root.as_path();
    let mut out = String::new();
    match &request.operation {
        Operation::List => {
            let _ = writeln!(out, "hash\t{}", root_hash(root)?); // writing to a String cannot fail
            for (file, name, requested, installed) in list_tools(mise, root)? {
                let _ = writeln!(
                    out,
                    "tool\t{file}\t{name}\t{requested}\t{}",
                    u8::from(installed)
                );
            }
        }
        Operation::Set(name, version, expected) => {
            let key = format!("tools.\"{name}\"");
            edit(root, expected, |file| {
                mise.run(root, &["config", "set", "-f", file, &key, version.as_str()])
            })?;
            let _ = writeln!(out, "hash\t{}", root_hash(root)?); // writing to a String cannot fail
        }
        Operation::Remove(name, expected) => {
            edit(root, expected, |file| {
                mise.run(root, &["unuse", "--no-prune", "-p", file, name.as_str()])
            })?;
            let _ = writeln!(out, "hash\t{}", root_hash(root)?); // writing to a String cannot fail
        }
        Operation::Registry => {
            let json = mise.run(root, &["registry", "--json"])?;
            for (name, description) in parse_registry(&json)? {
                let _ = writeln!(out, "reg\t{name}\t{description}"); // writing to a String cannot fail
            }
        }
        Operation::Versions(name) => {
            let listed = mise.run(root, &["ls-remote", name.as_str()])?;
            for version in newest(&listed) {
                let _ = writeln!(out, "ver\t{version}"); // writing to a String cannot fail
            }
        }
    }
    out.push_str("end\n");
    Ok(out)
}

/// The hash of the root `mise.toml`, or `absent`.
fn root_hash(root: &Path) -> Result<ToolsFileHash, MiseFailed> {
    match sha256_of(&root.join("mise.toml")) {
        Ok(hex) => ToolsFileHash::new(&hex).map_err(|e| MiseFailed(e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(ToolsFileHash::absent()),
        Err(e) => Err(MiseFailed(format!("could not read mise.toml: {e}"))),
    }
}

/// The copy of the root file that mise edits, beside the file it replaces.
const EDIT_COPY: &str = ".mise.toml.puddle-edit";

/// Edits the root `mise.toml` through `run`, which gets the path of a copy of it. The copy
/// replaces the file only if the file still has the hash the user looked at, so a save from the
/// editor that lands while mise runs is kept and the edit refused ([`Failure::Changed`]); mise
/// itself reads and writes the file without any such check. A link is edited where it points, and
/// stays a link.
fn edit(
    root: &Path,
    expected: &ToolsFileHash,
    run: impl FnOnce(&str) -> Result<String, MiseFailed>,
) -> Result<(), Failure> {
    guard(root, expected)?;
    let file = root.join("mise.toml");
    // No such file, or a link that leads nowhere (the guard above already read a link that
    // works): the link is refused, since writing through it would create what it points at.
    // On Windows the plain form (`C:\...`), not the `\\?\C:\...` that `std` returns: mise gets
    // the copy's path from it, and its errors show that path to the person.
    let target = dunce::canonicalize(&file).unwrap_or_else(|_| file.clone());
    if target == file && file.is_symlink() {
        return Err(MiseFailed("mise.toml is a link that leads nowhere".to_owned()).into());
    }
    let copy = target.with_file_name(EDIT_COPY);
    let made = if target.exists() {
        std::fs::copy(&target, &copy).map(drop)
    } else {
        std::fs::write(&copy, "")
    };
    made.map_err(|e| MiseFailed(format!("could not create mise.toml: {e}")))?;
    let swapped = run(&copy.to_string_lossy())
        .map_err(Failure::from)
        .and_then(|_| guard(root, expected))
        .and_then(|()| {
            std::fs::rename(&copy, &target)
                .map_err(|e| MiseFailed(format!("could not write mise.toml: {e}")).into())
        });
    if swapped.is_err() {
        // Best effort: the failure above is the one to report.
        let _ = std::fs::remove_file(&copy);
    }
    swapped
}

/// Refuses an edit when the root file is not the one the user looked at.
fn guard(root: &Path, expected: &ToolsFileHash) -> Result<(), Failure> {
    if &root_hash(root)? == expected {
        Ok(())
    } else {
        Err(Failure::Changed)
    }
}

/// `(file, name, requested version, installed)` for every tool of every mise file of the
/// workspace, files in the order [`config_folders`] finds them, then by name.
fn list_tools(
    mise: &impl Mise,
    root: &Path,
) -> Result<Vec<(String, String, String, bool)>, MiseFailed> {
    let mut seen = BTreeSet::new();
    for folder in config_folders(root) {
        for entry in parse_tools(&mise.run(&folder, &["ls", "--json"])?)? {
            if let Some(file) = relative(root, &entry.file) {
                seen.insert((file, entry.name, entry.requested, entry.installed));
            }
        }
    }
    let mut tools: Vec<_> = seen.into_iter().collect();
    // The root file first: it is the one the card edits.
    tools.sort_by_key(|(file, name, ..)| (file != "mise.toml", file.clone(), name.clone()));
    Ok(tools)
}

/// `file` below `root` as a `/` path, for the files of the workspace's own configs: not Puddle's
/// folder, no `..`, and not a file mise found elsewhere (the user's own, or the system's).
fn relative(root: &Path, file: &Path) -> Option<String> {
    let below = file.strip_prefix(root).ok()?;
    let parts: Vec<&str> = below
        .components()
        .map(|c| match c {
            Component::Normal(part) => part.to_str(),
            Component::Prefix(_)
            | Component::RootDir
            | Component::CurDir
            | Component::ParentDir => None,
        })
        .collect::<Option<_>>()?;
    let is_config = below.file_name().is_some_and(|f| {
        let f = f.to_string_lossy();
        CONFIG_FILES
            .iter()
            .any(|c| c.rsplit('/').next() == Some(&*f))
    });
    (is_config && parts.first().is_some_and(|p| *p != ".puddle")).then(|| parts.join("/"))
}

/// One tool of `mise ls --json`.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    file: PathBuf,
    name: String,
    requested: String,
    installed: bool,
}

/// The tools in `mise ls --json`: an object of tool name to a list of entries. An entry with no
/// config file, or whose name or version Puddle could not send back, is left out.
fn parse_tools(json: &str) -> Result<Vec<Entry>, MiseFailed> {
    #[derive(serde::Deserialize)]
    struct Source {
        path: PathBuf,
    }
    #[derive(serde::Deserialize)]
    struct Raw {
        #[serde(default)]
        requested_version: String,
        #[serde(default)]
        installed: bool,
        source: Option<Source>,
    }
    let tools: std::collections::BTreeMap<String, Vec<Raw>> = serde_json::from_str(json)
        .map_err(|e| MiseFailed(format!("mise ls printed something unexpected: {e}")))?;
    Ok(tools
        .into_iter()
        .filter(|(name, _)| ToolName::new(name).is_ok())
        .flat_map(|(name, raws)| {
            raws.into_iter().filter_map(move |raw| {
                let source = raw.source?;
                ToolVersion::new(&raw.requested_version).ok()?;
                Some(Entry {
                    file: source.path,
                    name: name.clone(),
                    requested: raw.requested_version,
                    installed: raw.installed,
                })
            })
        })
        .collect())
}

/// `(name, description)` of `mise registry --json`, descriptions cut and made one line. Names
/// Puddle could not send back are left out.
fn parse_registry(json: &str) -> Result<Vec<(String, String)>, MiseFailed> {
    #[derive(serde::Deserialize)]
    struct Raw {
        short: String,
        #[serde(default)]
        description: Option<String>,
    }
    let raws: Vec<Raw> = serde_json::from_str(json)
        .map_err(|e| MiseFailed(format!("mise registry printed something unexpected: {e}")))?;
    Ok(raws
        .into_iter()
        .filter(|r| ToolName::new(&r.short).is_ok())
        .take(MAX_REGISTRY)
        .map(|r| {
            let description = r
                .description
                .unwrap_or_default()
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .take(MAX_DESCRIPTION)
                .collect();
            (r.short, description)
        })
        .collect())
}

/// The newest [`MAX_VERSIONS`] versions of `mise ls-remote`, which lists oldest first, newest
/// first. Lines Puddle could not send back as a version are left out.
fn newest(listed: &str) -> Vec<&str> {
    listed
        .lines()
        .map(str::trim)
        .filter(|v| ToolVersion::new(v).is_ok())
        .rev()
        .take(MAX_VERSIONS)
        .collect()
}

#[cfg(test)]
mod tests;
