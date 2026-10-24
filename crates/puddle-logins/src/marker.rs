// SPDX-License-Identifier: GPL-3.0-or-later
//! A small file that says which services have a login kept for a workspace, so that a start
//! with capture off does not have to ask the credential store.
//!
//! Asking the store is what can hang: a locked or stuck keyring waits for the user. A workspace
//! with capture off keeps no login in use, so the only reason to ask is the notice that a login
//! kept earlier is no longer used. The marker answers that from a file. The store stays the
//! truth: the marker is written after the store is, and a marker that is missing, unreadable or
//! from another version says "unknown", which sends the start to the store once and then writes
//! the answer.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use puddle_types::WorkspaceName;

use crate::vault::workspace_hash;

/// The first line of a marker file; a file that starts otherwise is not read.
const HEADER: &str = "puddle-logins-kept v1";

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
    pub fn for_workspace(dir: &Path, workspace: &WorkspaceName) -> Self {
        Self::in_file(dir.join(format!("{}.kept", workspace_hash(workspace.as_str()))))
    }

    /// No marker: every start with capture off asks the credential store.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
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
        let text = std::fs::read_to_string(self.path.as_ref()?).ok()?;
        let mut lines = text.lines();
        (lines.next()? == HEADER).then(|| {
            lines
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()
        })
    }

    /// Records `kept`. A marker that cannot be written is only logged: the next start asks the
    /// store again, which is the cost of not having one.
    pub(crate) fn write(&self, kept: &BTreeSet<String>) {
        let Some(path) = &self.path else {
            return;
        };
        let mut text = String::from(HEADER);
        for id in kept {
            text.push('\n');
            text.push_str(id);
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
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn what_was_written_is_read_back_including_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let marker = Marker::in_file(dir.path().join("deeper").join("one.kept"));
        assert_eq!(marker.read(), None);
        marker.write(&set(&["claude", "github"]));
        assert_eq!(marker.read(), Some(set(&["claude", "github"])));
        marker.write(&set(&[]));
        assert_eq!(marker.read(), Some(set(&[])));
    }

    #[test]
    fn a_file_that_is_not_ours_or_is_from_another_version_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.kept");
        let marker = Marker::in_file(path.clone());
        std::fs::write(&path, "claude\n").unwrap();
        assert_eq!(marker.read(), None);
        std::fs::write(&path, "puddle-logins-kept v9\nclaude\n").unwrap();
        assert_eq!(marker.read(), None);
        std::fs::write(&path, b"").unwrap();
        assert_eq!(marker.read(), None);
    }

    #[test]
    fn no_marker_never_knows_and_a_folder_that_cannot_be_made_only_costs_the_marker() {
        let none = Marker::none();
        none.write(&set(&["claude"]));
        assert_eq!(none.read(), None);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a-file");
        std::fs::write(&file, "x").unwrap();
        let blocked = Marker::in_file(file.join("below").join("one.kept"));
        blocked.write(&set(&["claude"]));
        assert_eq!(blocked.read(), None);
    }
}
