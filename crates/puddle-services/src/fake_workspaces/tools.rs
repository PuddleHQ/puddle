// SPDX-License-Identifier: AGPL-3.0-or-later
//! The managed tools of the fake workspaces: a root `mise.toml` per workspace that the edits
//! change, with the answers the real service gives (the hash, the `409`s, the stopped listing).

use std::collections::{BTreeMap, BTreeSet};

use puddle_types::{ToolName, ToolVersion, ToolsFileHash, WorkspaceId};

use crate::wire::{RegistryTool, ToolEntry, ToolRegistry, ToolVersions, ToolsFile, ToolsListing};
use crate::workspaces::WorkspaceError;

/// What the root file of one workspace holds.
#[derive(Debug, Clone, Default)]
struct RootFile {
    tools: BTreeMap<ToolName, (ToolVersion, bool)>,
    /// Counts the changes; the hash is made from it.
    revision: u64,
}

impl RootFile {
    fn hash(&self) -> ToolsFileHash {
        ToolsFileHash::new(&format!("{:064x}", self.revision))
            .unwrap_or_else(|_| ToolsFileHash::absent())
    }
}

/// The tools of every fake workspace, and what the edits did.
#[derive(Debug, Default)]
pub(super) struct FakeTools {
    off: BTreeSet<WorkspaceId>,
    roots: BTreeMap<WorkspaceId, RootFile>,
    repo_files: BTreeMap<WorkspaceId, Vec<ToolsFile>>,
    last: BTreeMap<WorkspaceId, ToolsListing>,
    registry: Vec<RegistryTool>,
    registry_problem: Option<String>,
    versions: BTreeMap<ToolName, Result<Vec<ToolVersion>, String>>,
    installs: Vec<WorkspaceId>,
}

const STOPPED: &str = "the workspace is not running; start it to change tools";
const OFF: &str = "tools are off for this workspace";

impl FakeTools {
    pub(super) fn set_off(&mut self, id: &WorkspaceId, off: bool) {
        if off {
            self.off.insert(id.clone());
        } else {
            self.off.remove(id);
        }
    }

    pub(super) fn set_root(&mut self, id: &WorkspaceId, tools: Vec<(ToolName, ToolVersion, bool)>) {
        let root = self.roots.entry(id.clone()).or_default();
        root.tools = tools.into_iter().map(|(n, v, i)| (n, (v, i))).collect();
        root.revision += 1;
    }

    pub(super) fn hand_edit(&mut self, id: &WorkspaceId) {
        self.roots.entry(id.clone()).or_default().revision += 1;
    }

    pub(super) fn set_repo_files(&mut self, id: &WorkspaceId, files: Vec<ToolsFile>) {
        self.repo_files.insert(id.clone(), files);
    }

    pub(super) fn set_registry(&mut self, tools: Vec<RegistryTool>, problem: Option<String>) {
        self.registry = tools;
        self.registry_problem = problem;
    }

    pub(super) fn set_versions(
        &mut self,
        name: ToolName,
        answer: Result<Vec<ToolVersion>, String>,
    ) {
        self.versions.insert(name, answer);
    }

    pub(super) fn installs(&self) -> Vec<WorkspaceId> {
        self.installs.clone()
    }

    fn fresh(&self, id: &WorkspaceId) -> ToolsListing {
        let root = self.roots.get(id);
        let mut files = Vec::new();
        if let Some(root) = root.filter(|r| !r.tools.is_empty()) {
            files.push(ToolsFile {
                path: "mise.toml".to_owned(),
                root: true,
                tools: root
                    .tools
                    .iter()
                    .map(|(name, (version, installed))| ToolEntry {
                        name: name.clone(),
                        requested: version.clone(),
                        installed: *installed,
                    })
                    .collect(),
            });
        }
        files.extend(self.repo_files.get(id).cloned().unwrap_or_default());
        ToolsListing {
            running: true,
            listed_at: None,
            problem: None,
            sha256: Some(root.map_or_else(ToolsFileHash::absent, RootFile::hash)),
            files,
        }
    }

    /// The listing a call answers: fresh when `running`, else the last one with the reason.
    pub(super) fn listing(&mut self, id: &WorkspaceId, running: bool, now: u64) -> ToolsListing {
        if self.off.contains(id) {
            return ToolsListing {
                running,
                listed_at: None,
                problem: Some(OFF.to_owned()),
                sha256: None,
                files: Vec::new(),
            };
        }
        if !running {
            let mut last = self.last.get(id).cloned().unwrap_or(ToolsListing {
                running: false,
                listed_at: None,
                problem: None,
                sha256: None,
                files: Vec::new(),
            });
            last.running = false;
            last.problem = Some(STOPPED.to_owned());
            return last;
        }
        let mut fresh = self.fresh(id);
        fresh.listed_at = Some(now);
        self.last.insert(id.clone(), fresh.clone());
        fresh
    }

    /// The refusal for an edit or a lookup the workspace cannot take now.
    pub(super) fn require(&self, id: &WorkspaceId, running: bool) -> Result<(), WorkspaceError> {
        if !running {
            Err(WorkspaceError::conflict(STOPPED))
        } else if self.off.contains(id) {
            Err(WorkspaceError::conflict(OFF))
        } else {
            Ok(())
        }
    }

    fn checked_root(
        &mut self,
        id: &WorkspaceId,
        hash: &ToolsFileHash,
    ) -> Result<&mut RootFile, WorkspaceError> {
        let root = self.roots.entry(id.clone()).or_default();
        let current = if root.revision == 0 {
            ToolsFileHash::absent()
        } else {
            root.hash()
        };
        if &current == hash {
            Ok(root)
        } else {
            Err(WorkspaceError::conflict(
                "mise.toml changed since it was listed; list the tools again",
            ))
        }
    }

    pub(super) fn set(
        &mut self,
        id: &WorkspaceId,
        name: &ToolName,
        version: &ToolVersion,
        hash: &ToolsFileHash,
    ) -> Result<(), WorkspaceError> {
        let root = self.checked_root(id, hash)?;
        root.tools.insert(name.clone(), (version.clone(), false));
        root.revision += 1;
        self.installs.push(id.clone());
        Ok(())
    }

    pub(super) fn remove(
        &mut self,
        id: &WorkspaceId,
        name: &ToolName,
        hash: &ToolsFileHash,
    ) -> Result<(), WorkspaceError> {
        let root = self.checked_root(id, hash)?;
        root.tools.remove(name);
        root.revision += 1;
        self.installs.push(id.clone());
        Ok(())
    }

    pub(super) fn registry(&self) -> ToolRegistry {
        ToolRegistry {
            tools: self.registry.clone(),
            problem: self.registry_problem.clone(),
        }
    }

    pub(super) fn versions(&self, name: &ToolName) -> ToolVersions {
        match self.versions.get(name) {
            Some(Ok(versions)) => ToolVersions {
                versions: versions.clone(),
                reason: None,
            },
            Some(Err(reason)) => ToolVersions {
                versions: Vec::new(),
                reason: Some(reason.clone()),
            },
            None => ToolVersions {
                versions: Vec::new(),
                reason: None,
            },
        }
    }

    pub(super) fn install(&mut self, id: &WorkspaceId) {
        self.installs.push(id.clone());
    }
}
