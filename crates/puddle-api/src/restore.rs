// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore from a backup file: the types of the preview, the [`RestoreService`] trait the routes
//! call, [`NoRestore`] for a build without it, and [`FakeRestore`] for tests and the UI fixture.
//!
//! Reading and checking the file is the host's (`puddle-host`, `restore`); this crate carries the
//! upload to it and adds what only the API knows (which workspaces run, which disks exist, which
//! secrets have a value here) to the preview it gets back.

use std::sync::{Mutex, PoisonError};

use crate::wire::{RestorePart, RestorePartName};
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;

/// The body of an upload, in pieces; a read error ends it with that error.
pub type UploadStream = BoxStream<'static, std::io::Result<Vec<u8>>>;

/// Why a restore request was not served.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RestoreServiceError {
    /// Not wired in this build or state (503).
    #[error("{0}")]
    Unavailable(String),
    /// The file cannot be read faithfully, and why (422). Nothing was staged.
    #[error("{0}")]
    Refused(String),
    /// The staging id is unknown, or its staging folder is gone (404).
    #[error("{0}")]
    NotStaged(String),
    /// The server could not read or write what it needs (503, with the reason).
    #[error("{0}")]
    Failed(String),
}

/// A secret variable of the backup and the id its value would have in the credential store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretRef {
    /// The workspace, or none for a global variable.
    pub workspace_id: Option<String>,
    /// The variable's name.
    pub name: String,
    /// The credential-store id of its value.
    pub secret_id: String,
}

/// What the service knows about a staged file; the route adds the rest to make a [`RestorePreview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreFacts {
    /// The staging id.
    pub staging_id: String,
    /// The backup format.
    pub format: u32,
    /// The Puddle that made it.
    pub puddle_version: Option<String>,
    /// The database's schema version.
    pub schema_version: u32,
    /// When it was made.
    pub created_at_ms: Option<u64>,
    /// The part counts.
    pub parts: Vec<RestorePart>,
    /// The workspaces it lists: id and name.
    pub workspaces: Vec<(String, String)>,
    /// Its secret variables.
    pub secret_refs: Vec<SecretRef>,
}

/// Reads and checks a backup file for a restore.
pub trait RestoreService: Send + Sync {
    /// Reads the uploaded archive into a private staging folder, checks it and compares it with
    /// this machine. Changes nothing that exists.
    fn preview(
        &self,
        upload: UploadStream,
    ) -> BoxFuture<'_, Result<RestoreFacts, RestoreServiceError>>;
}

/// The service when none is wired in: every call says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoRestore;

impl RestoreService for NoRestore {
    fn preview(&self, _: UploadStream) -> BoxFuture<'_, Result<RestoreFacts, RestoreServiceError>> {
        Box::pin(async {
            Err(RestoreServiceError::Unavailable(
                "restore is not available in this build".into(),
            ))
        })
    }
}

/// An in-memory [`RestoreService`] for tests and the UI fixture: it accepts a body that starts with
/// [`FAKE_BACKUP_BYTES`]'s first word, refuses anything else as a file that is not a backup, and
/// answers with the facts set with [`FakeRestore::set_facts`].
#[derive(Debug)]
pub struct FakeRestore {
    state: Mutex<FakeState>,
}

#[derive(Debug)]
struct FakeState {
    facts: RestoreFacts,
    previews: usize,
    last_upload_bytes: usize,
}

/// The first bytes a body must have for [`FakeRestore`] to accept it.
pub const FAKE_RESTORE_MAGIC: &[u8] = b"Puddle fake backup";

impl Default for FakeRestore {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRestore {
    /// A fake that answers with one rule to add and nothing else.
    #[must_use]
    pub fn new() -> Self {
        let part = |name, in_backup, add| RestorePart {
            name,
            in_backup,
            add,
            differ: 0,
            same: in_backup - add,
            only_here: 0,
        };
        Self {
            state: Mutex::new(FakeState {
                facts: RestoreFacts {
                    staging_id: "0".repeat(32),
                    format: 1,
                    puddle_version: Some("0.0.0".into()),
                    schema_version: 1,
                    created_at_ms: Some(0),
                    parts: vec![part(RestorePartName::Rules, 1, 1)],
                    workspaces: Vec::new(),
                    secret_refs: Vec::new(),
                },
                previews: 0,
                last_upload_bytes: 0,
            }),
        }
    }

    /// Makes every accepted preview answer with `facts`.
    pub fn set_facts(&self, facts: RestoreFacts) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .facts = facts;
    }

    /// How many previews were asked for.
    #[must_use]
    pub fn previews(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .previews
    }

    /// How many bytes the last upload had.
    #[must_use]
    pub fn last_upload_bytes(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last_upload_bytes
    }
}

impl RestoreService for FakeRestore {
    fn preview(
        &self,
        upload: UploadStream,
    ) -> BoxFuture<'_, Result<RestoreFacts, RestoreServiceError>> {
        Box::pin(async move {
            use futures_util::StreamExt as _;
            let mut upload = upload;
            let mut body = Vec::new();
            while let Some(piece) = upload.next().await {
                body.extend(piece.map_err(|e| {
                    RestoreServiceError::Failed(format!("the upload stopped: {e}"))
                })?);
            }
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.previews += 1;
            state.last_upload_bytes = body.len();
            if !body.starts_with(FAKE_RESTORE_MAGIC) {
                return Err(RestoreServiceError::Refused(
                    "this is not a gzip archive; choose a file made by Puddle's \"Back up now\""
                        .into(),
                ));
            }
            Ok(state.facts.clone())
        })
    }
}

#[cfg(test)]
mod tests;
