// SPDX-License-Identifier: AGPL-3.0-or-later
//! The workspaces whose disk a quit could not trim, kept in a small file so the next start can
//! tell the user. The quit has no window left to say it in, and the log is not where a user looks.

use std::collections::BTreeMap;
use std::path::PathBuf;

use puddle_types::{Problem, WorkspaceId};
use serde::{Deserialize, Serialize};

/// The file's format version; a file with another one was not written by this Puddle.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    /// Each workspace id with why its trim failed.
    untrimmed: BTreeMap<String, String>,
}

/// The file in the data folder.
#[derive(Debug, Clone)]
pub(crate) struct QuitTrims {
    path: PathBuf,
}

impl QuitTrims {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// What the file says; nothing when there is no file. A file that is unreadable, not
    /// valid, of another version or naming an id that is no workspace id is ignored (with a log
    /// line): it only carries a notice, so it must never stop a start.
    pub(crate) fn load(&self) -> BTreeMap<WorkspaceId, String> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return BTreeMap::new(),
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "the trim notes of the last quit cannot be read and are ignored");
                return BTreeMap::new();
            }
        };
        let file: File = match serde_json::from_slice(&bytes) {
            Ok(file) => file,
            Err(e) => {
                tracing::warn!(path = %self.path.display(), error = %e, "the trim notes of the last quit are not valid and are ignored");
                return BTreeMap::new();
            }
        };
        if file.version != VERSION {
            tracing::warn!(path = %self.path.display(), version = file.version, "the trim notes of the last quit are of another version and are ignored");
            return BTreeMap::new();
        }
        file.untrimmed
            .into_iter()
            .filter_map(|(id, why)| match WorkspaceId::new(&id) {
                Ok(id) => Some((id, why)),
                Err(e) => {
                    tracing::warn!(error = %e, "a trim note names no workspace and is ignored");
                    None
                }
            })
            .collect()
    }

    /// Makes the file say `untrimmed`; no entries removes the file. A failure is logged: the
    /// quit goes on, and the disk is trimmed by the next stop anyway.
    pub(crate) fn save(&self, untrimmed: &BTreeMap<WorkspaceId, String>) {
        let result = if untrimmed.is_empty() {
            match std::fs::remove_file(&self.path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(SaveError::Io(e)),
                _ => Ok(()),
            }
        } else {
            let file = File {
                version: VERSION,
                untrimmed: untrimmed
                    .iter()
                    .map(|(id, why)| (id.to_string(), why.clone()))
                    .collect(),
            };
            serde_json::to_vec_pretty(&file)
                .map_err(SaveError::Encode)
                .and_then(|body| {
                    puddle_fs::private::write_atomic(&self.path, &body).map_err(SaveError::Io)
                })
        };
        if let Err(error) = result {
            tracing::warn!(path = %self.path.display(), %error, "the trim notes of this quit were not saved");
        }
    }
}

/// Why the trim notes were not saved.
#[derive(Debug, thiserror::Error)]
enum SaveError {
    /// The file could not be written or removed.
    #[error(transparent)]
    Io(std::io::Error),
    /// The notes could not be encoded.
    #[error(transparent)]
    Encode(serde_json::Error),
}

/// The problem key of a workspace's untrimmed disk (the one a failed trim at a stop raises).
pub(crate) fn key(workspace: &WorkspaceId) -> String {
    format!("trim:{workspace}")
}

/// The problem a trim that failed when Puddle quit raises at the next start.
pub(crate) fn problem(workspace: &WorkspaceId, name: &str, why: &str) -> Problem {
    Problem::new(
        key(workspace),
        format!("{name}'s disk was not trimmed when Puddle quit"),
        format!(
            "{why}. The workspace stopped with Puddle. Its disk file stays larger than the data \
             in it until a later stop, or Reclaim space, trims it."
        ),
    )
    .about(workspace.clone())
}

#[cfg(test)]
mod tests;
