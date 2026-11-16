// SPDX-License-Identifier: GPL-3.0-or-later
//! The Tools card's calls: what the workspace's mise files ask for, edits of the root `mise.toml`,
//! the registry and a tool's versions, all through `puddle-agent mise-tools` in the guest as the
//! session user (the guest side is `puddle_agent::mise_tools`).
//!
//! The guest's answers are hostile input: [`records`] takes lines of tab-separated fields with an
//! `end` last, bounded, no control characters, and every parser below takes one exact shape per
//! record, validating names and versions into their types. An answer that is anything else is
//! refused whole.
//!
//! A stopped workspace answers the last listing of this run with its time, like the checkouts
//! do. An edit that changed the file starts the install in the background and answers the
//! listing that follows.

use std::time::Duration;

use puddle_api::WorkspaceError;
use puddle_api::wire::{
    RegistryTool, ToolEntry, ToolRegistry, ToolVersions, ToolsFile, ToolsListing,
};
use puddle_compute::{ComputeError, ExecOutput, ExecRequest, Runtime, Sandbox as _};
use puddle_settings::MiseMode;
use puddle_types::{ToolName, ToolVersion, ToolsFileHash, WorkspaceId, WorkspaceStatus};
use puddle_workspace::WORKSPACES_ROOT;

use super::HostWorkspaces;
use crate::workspaces::not_found;

/// How long a listing may take: mise resolves versions on the way, and may ask the network.
const LIST_TIMEOUT: Duration = Duration::from_mins(2);

/// How long an edit, the registry or a version list may take.
const CALL_TIMEOUT: Duration = Duration::from_mins(2);

/// The most records an answer may hold, and the longest field.
const MAX_RECORDS: usize = 5100;
const MAX_FIELD: usize = 500;

/// The exit code of `mise-tools` for a root file that changed since the listing.
const CHANGED_EXIT: i32 = 5;

const STOPPED: &str = "the workspace is not running; start it to change tools";
const OFF: &str = "tools are off for this workspace";

/// The last listing made of one workspace in this run.
#[derive(Debug, Clone)]
pub(super) struct Listed {
    at: u64,
    hash: ToolsFileHash,
    files: Vec<ToolsFile>,
}

/// The lines of `stdout` up to `end` as tab-separated fields. `None` for no `end`, anything but
/// blank lines after it, more than `max` records, or a field that is too long or holds a control
/// character.
fn records(stdout: &str, max: usize) -> Option<Vec<Vec<&str>>> {
    let mut lines = stdout.lines();
    let mut out = Vec::new();
    let mut ended = false;
    for line in lines.by_ref() {
        if line == "end" {
            ended = true;
            break;
        }
        if out.len() >= max {
            return None;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        let clean = fields
            .iter()
            .all(|f| f.chars().count() <= MAX_FIELD && !f.chars().any(char::is_control));
        if !clean {
            return None;
        }
        out.push(fields);
    }
    (ended && lines.all(|l| l.trim().is_empty())).then_some(out)
}

/// `list`: the root file's hash, then `tool` records, grouped by file in order of appearance.
fn parse_listing(stdout: &str) -> Option<(ToolsFileHash, Vec<ToolsFile>)> {
    let mut records = records(stdout, MAX_RECORDS)?.into_iter();
    let hash = match records.next()?.as_slice() {
        ["hash", hash] => ToolsFileHash::new(hash).ok()?,
        _ => return None,
    };
    let mut files: Vec<ToolsFile> = Vec::new();
    for record in records {
        let ["tool", file, name, requested, installed] = record.as_slice() else {
            return None;
        };
        let installed = match *installed {
            "1" => true,
            "0" => false,
            _ => return None,
        };
        if file.is_empty() {
            return None;
        }
        let entry = ToolEntry {
            name: ToolName::new(name).ok()?,
            requested: ToolVersion::new(requested).ok()?,
            installed,
        };
        match files.iter_mut().find(|f| f.path == *file) {
            Some(known) => known.tools.push(entry),
            None => files.push(ToolsFile {
                path: (*file).to_owned(),
                root: *file == "mise.toml",
                tools: vec![entry],
            }),
        }
    }
    Some((hash, files))
}

/// `set` and `remove`: the root file's hash after the edit.
fn parse_hash(stdout: &str) -> Option<ToolsFileHash> {
    match records(stdout, 1)?.as_slice() {
        [fields] => match fields.as_slice() {
            ["hash", hash] => ToolsFileHash::new(hash).ok(),
            _ => None,
        },
        _ => None,
    }
}

/// `registry`: `reg` records of a name and a description.
fn parse_registry(stdout: &str) -> Option<Vec<RegistryTool>> {
    records(stdout, MAX_RECORDS)?
        .into_iter()
        .map(|record| match record.as_slice() {
            ["reg", name, description] => Some(RegistryTool {
                name: ToolName::new(name).ok()?,
                description: (*description).to_owned(),
            }),
            _ => None,
        })
        .collect()
}

/// `versions`: `ver` records, newest first.
fn parse_versions(stdout: &str) -> Option<Vec<ToolVersion>> {
    records(stdout, 200)?
        .into_iter()
        .map(|record| match record.as_slice() {
            ["ver", version] => ToolVersion::new(version).ok(),
            _ => None,
        })
        .collect()
}

/// Why a call did not give an answer.
#[derive(Debug, PartialEq, Eq)]
enum Failed {
    /// The root file is not the one the user looked at.
    Changed,
    /// mise or the guest said no, or the answer was unreadable: the reason, one line.
    Reason(String),
}

/// The answer in `outcome` as `parse` reads it.
fn read<T>(
    outcome: Result<ExecOutput, ComputeError>,
    parse: impl FnOnce(&str) -> Option<T>,
) -> Result<T, Failed> {
    let out = outcome.map_err(|e| Failed::Reason(e.to_string()))?;
    if out.status.code == CHANGED_EXIT {
        return Err(Failed::Changed);
    }
    if out.status.code != 0 {
        let reason = out
            .stderr_text()
            .lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map_or_else(
                || format!("mise ended with exit code {}", out.status.code),
                str::to_owned,
            );
        return Err(Failed::Reason(reason));
    }
    parse(&out.stdout_text())
        .ok_or_else(|| Failed::Reason("the guest's answer could not be read".to_owned()))
}

/// What a listing answers: `listed` when there is one to show.
fn view(running: bool, listed: Option<&Listed>, problem: Option<String>) -> ToolsListing {
    ToolsListing {
        running,
        listed_at: listed.map(|l| l.at),
        problem,
        sha256: listed.map(|l| l.hash.clone()),
        files: listed.map(|l| l.files.clone()).unwrap_or_default(),
    }
}

impl<R: Runtime + Clone> HostWorkspaces<R> {
    fn workspace_status(&self, id: &WorkspaceId) -> Result<WorkspaceStatus, WorkspaceError> {
        self.state()
            .slots
            .get(id)
            .map(|s| s.record.status)
            .ok_or_else(|| not_found(id))
    }

    fn tools_mode(&self, id: &WorkspaceId) -> Result<MiseMode, WorkspaceError> {
        crate::settings_read::mise(self.inner.settings.as_ref(), id)
            .map_err(WorkspaceError::unavailable)
    }

    /// Fails with a conflict unless the workspace runs with the layer on.
    fn require_tools(&self, id: &WorkspaceId) -> Result<(), WorkspaceError> {
        if self.workspace_status(id)? != WorkspaceStatus::Running {
            return Err(WorkspaceError::conflict(STOPPED));
        }
        match self.tools_mode(id)? {
            MiseMode::Off => Err(WorkspaceError::conflict(OFF)),
            MiseMode::On { .. } => Ok(()),
        }
    }

    /// Runs `puddle-agent <args>` in the guest as the session user. `None` when the workspace is
    /// not running, at the start or at the end.
    pub(super) async fn exec_agent(
        &self,
        name: &WorkspaceId,
        args: Vec<String>,
        timeout: Duration,
    ) -> Option<Result<ExecOutput, ComputeError>> {
        let inner = &self.inner;
        let gated = inner.live.lock().await.get(name)?.gated.clone()?;
        let user = self.booted_user(name).await;
        let request = ExecRequest::new(puddle_boot::AGENT_GUEST, args)
            .as_user(user.name())
            .with_timeout(timeout);
        let outcome = gated.ungated().exec(request).await;
        // A stop keeps the entry but takes its sandbox away.
        let running = inner
            .live
            .lock()
            .await
            .get(name)
            .is_some_and(|l| l.gated.is_some());
        running.then_some(outcome)
    }

    /// `puddle-agent mise-tools <workspace-dir> <words...>`.
    async fn mise_tools(
        &self,
        name: &WorkspaceId,
        words: &[&str],
        timeout: Duration,
    ) -> Option<Result<ExecOutput, ComputeError>> {
        let mut args = vec!["mise-tools".to_owned(), format!("{WORKSPACES_ROOT}/{name}")];
        args.extend(words.iter().map(|w| (*w).to_owned()));
        self.exec_agent(name, args, timeout).await
    }

    /// The tools of workspace `id`: listed now when it runs with the layer on, else the last
    /// listing of this run with the reason. A listing that fails is the problem, not an error.
    pub(crate) async fn tools_of(&self, id: &WorkspaceId) -> Result<ToolsListing, WorkspaceError> {
        let running = self.workspace_status(id)? == WorkspaceStatus::Running;
        let kept = self.inner.tools.listed(id);
        if matches!(self.tools_mode(id)?, MiseMode::Off) {
            return Ok(view(running, None, Some(OFF.to_owned())));
        }
        if !running {
            return Ok(view(false, kept.as_ref(), Some(STOPPED.to_owned())));
        }
        let Some(outcome) = self.mise_tools(id, &["list"], LIST_TIMEOUT).await else {
            return Ok(view(false, kept.as_ref(), Some(STOPPED.to_owned())));
        };
        match read(outcome, parse_listing) {
            Ok((hash, files)) => {
                let listed = Listed {
                    at: self.inner.clock.now_ms(),
                    hash,
                    files,
                };
                let shown = view(true, Some(&listed), None);
                self.inner.tools.keep_listed(id, listed);
                Ok(shown)
            }
            Err(failed) => {
                let reason = reason_of(failed);
                tracing::warn!(workspace = %id, %reason, "the tools could not be listed");
                Ok(view(true, None, Some(reason)))
            }
        }
    }

    /// Sets `name` to `version` in the root file, if its hash is `hash`.
    pub(crate) async fn set_tool_in(
        &self,
        id: &WorkspaceId,
        name: &ToolName,
        version: &ToolVersion,
        hash: &ToolsFileHash,
    ) -> Result<ToolsListing, WorkspaceError> {
        self.edit_root(id, &["set", name.as_str(), version.as_str(), hash.as_str()])
            .await
    }

    /// Takes `name` out of the root file, if its hash is `hash`.
    pub(crate) async fn remove_tool_in(
        &self,
        id: &WorkspaceId,
        name: &ToolName,
        hash: &ToolsFileHash,
    ) -> Result<ToolsListing, WorkspaceError> {
        self.edit_root(id, &["remove", name.as_str(), hash.as_str()])
            .await
    }

    async fn edit_root(
        &self,
        id: &WorkspaceId,
        words: &[&str],
    ) -> Result<ToolsListing, WorkspaceError> {
        self.require_tools(id)?;
        let outcome = self
            .mise_tools(id, words, CALL_TIMEOUT)
            .await
            .ok_or_else(|| WorkspaceError::conflict(STOPPED))?;
        match read(outcome, parse_hash) {
            Ok(_) => {
                self.inner.tools.forget_listed(id);
                // The new line is not installed yet: the install runs behind this answer.
                self.set_up_tools_later(id);
                self.tools_of(id).await
            }
            Err(Failed::Changed) => Err(WorkspaceError::conflict(
                "mise.toml changed since it was listed; list the tools again",
            )),
            Err(Failed::Reason(reason)) => Err(WorkspaceError::unavailable(format!(
                "mise could not change mise.toml: {reason}"
            ))),
        }
    }

    /// The tools mise knows by name: one answer for the life of the process, since the pinned
    /// mise (and so its registry) is fixed in it.
    pub(crate) async fn tool_registry_of(
        &self,
        id: &WorkspaceId,
    ) -> Result<ToolRegistry, WorkspaceError> {
        self.require_tools(id)?;
        if let Some(tools) = self.inner.tools.registry() {
            return Ok(ToolRegistry {
                tools,
                problem: None,
            });
        }
        let outcome = self
            .mise_tools(id, &["registry"], CALL_TIMEOUT)
            .await
            .ok_or_else(|| WorkspaceError::conflict(STOPPED))?;
        Ok(match read(outcome, parse_registry) {
            Ok(tools) => {
                self.inner.tools.keep_registry(tools.clone());
                ToolRegistry {
                    tools,
                    problem: None,
                }
            }
            Err(failed) => ToolRegistry {
                tools: Vec::new(),
                problem: Some(reason_of(failed)),
            },
        })
    }

    /// The newest versions mise lists for `name`; an empty list with the reason when it cannot.
    pub(crate) async fn tool_versions_of(
        &self,
        id: &WorkspaceId,
        name: &ToolName,
    ) -> Result<ToolVersions, WorkspaceError> {
        self.require_tools(id)?;
        let outcome = self
            .mise_tools(id, &["versions", name.as_str()], CALL_TIMEOUT)
            .await
            .ok_or_else(|| WorkspaceError::conflict(STOPPED))?;
        Ok(match read(outcome, parse_versions) {
            Ok(versions) => ToolVersions {
                versions,
                reason: None,
            },
            Err(failed) => ToolVersions {
                versions: Vec::new(),
                reason: Some(reason_of(failed)),
            },
        })
    }

    /// Starts the install in the background: the retry of a failed one.
    pub(crate) fn request_tool_install(&self, id: &WorkspaceId) -> Result<(), WorkspaceError> {
        self.require_tools(id)?;
        self.set_up_tools_later(id);
        Ok(())
    }
}

/// What a failed call says to the user.
fn reason_of(failed: Failed) -> String {
    match failed {
        Failed::Reason(reason) => reason,
        Failed::Changed => "the guest's answer could not be read".to_owned(),
    }
}

#[cfg(test)]
mod tests;
