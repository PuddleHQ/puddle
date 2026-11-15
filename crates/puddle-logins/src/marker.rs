// SPDX-License-Identifier: AGPL-3.0-or-later
//! A small file that says which services have a login kept for a workspace, so that a start
//! with capture off does not have to ask the credential store.
//!
//! Asking the store is what can hang: a locked or stuck keyring waits for the user. A workspace
//! with capture off keeps no login in use, so the only reason to ask is the notice that a login
//! kept earlier is no longer used. The marker answers that from a file. The store stays the
//! truth: the marker is written after the store is, and a marker that is missing, unreadable or
//! from another version says "unknown", which sends the start to the store once and then writes
//! the answer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use puddle_types::WorkspaceId;

use crate::vault::workspace_hash;

/// The first line of a marker file; a file that starts otherwise is not read. Version 2 adds the
/// time each service was first noted as kept (`claude 1761480000000`, epoch ms); an older build
/// reads such a file as unknown and asks the store, which is the safe way to fail.
const HEADER: &str = "puddle-logins-kept v2";

/// The header of the first version, which had no times.
const HEADER_V1: &str = "puddle-logins-kept v1";

/// The services a marker names, each with the time it was first noted as kept when that is known.
pub(crate) type KeptSince = BTreeMap<String, Option<u64>>;

/// Where one workspace's marker lives, or nowhere.
#[derive(Debug, Clone, Default)]
pub struct Marker {
    path: Option<PathBuf>,
}

impl Marker {
    /// A marker kept in the file `path` (its folder is made when the first write needs it).
    #[must_use]
    pub fn in_file(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    /// The marker of `workspace` in the folder `dir`: one file named by a hash of the workspace's
    /// name, so no name can reach outside the folder.
    #[must_use]
    pub fn for_workspace(dir: &Path, workspace: &WorkspaceId) -> Self {
        Self::in_file(dir.join(format!("{}.kept", workspace_hash(workspace.as_str()))))
    }

    /// No marker: every start with capture off asks the credential store.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// The file the marker is kept in, if any.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether the file is there. One that cannot be looked for counts as there; a marker in no
    /// file never is.
    #[must_use]
    pub fn exists(&self) -> bool {
        self.path
            .as_deref()
            .is_some_and(|path| !matches!(path.try_exists(), Ok(false)))
    }

    /// Deletes the file, when a workspace is deleted. A file that is not there is fine.
    pub fn clear(&self) {
        if let Some(path) = &self.path
            && let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "the marker of a deleted workspace's saved logins could not be removed");
        }
    }

    /// The services that have a login kept, or `None` when that is not known.
    pub(crate) fn read(&self) -> Option<BTreeSet<String>> {
        self.read_since().map(|kept| kept.into_keys().collect())
    }

    /// The same with the time each was first noted as kept (`None` for a service noted before
    /// times were written, or whose time is not a number).
    pub(crate) fn read_since(&self) -> Option<KeptSince> {
        let text = std::fs::read_to_string(self.path.as_ref()?).ok()?;
        let mut lines = text.lines();
        let header = lines.next()?;
        (header == HEADER || header == HEADER_V1).then(|| {
            lines
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| match line.split_once(' ') {
                    Some((id, since)) => (id.to_owned(), since.trim().parse().ok()),
                    None => (line.to_owned(), None),
                })
                .collect()
        })
    }

    /// Records `kept`. A marker that cannot be written is only logged: the next start asks the
    /// store again, which is the cost of not having one.
    ///
    /// A service the marker already named keeps its time. One that is new gets the time now when
    /// the marker was known (it became kept since), and no time when it was not (when it became
    /// kept is not known).
    pub(crate) fn write(&self, kept: &BTreeSet<String>) {
        let Some(path) = &self.path else {
            return;
        };
        let before = self.read_since();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok());
        let mut text = String::from(HEADER);
        for id in kept {
            text.push('\n');
            text.push_str(id);
            let since = match &before {
                Some(before) => before.get(id).copied().unwrap_or(now),
                None => None,
            };
            if let Some(since) = since {
                text.push(' ');
                text.push_str(&since.to_string());
            }
        }
        text.push('\n');
        let result = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                // Written beside the marker and moved over it, so a start never reads half of one.
                let part = path.with_extension("part");
                std::fs::write(&part, text)?;
                std::fs::rename(&part, path)
            });
        if let Err(error) = result {
            tracing::warn!(%error, "the marker of a workspace's saved logins could not be written");
        }
    }
}

#[cfg(test)]
mod tests;
