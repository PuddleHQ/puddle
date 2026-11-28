// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore from a backup file: the types of the preview, the [`RestoreService`] trait the routes
//! call, [`NoRestore`] for a build without it, and [`FakeRestore`] for tests and the UI fixture.
//!
//! Reading and checking the file is the host's (`puddle-host`, `restore`); this crate carries the
//! upload to it and adds what only the API knows (which workspaces run, which disks exist, which
//! secrets have a value here) to the preview it gets back.

#[cfg(any(test, feature = "testing"))]
use std::sync::{Mutex, PoisonError};

#[cfg(any(test, feature = "testing"))]
use crate::wire::RestorePartName;
use crate::wire::{RestoreApplied, RestoreMode, RestorePart, RestoreResult, SafetyCopy};
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

/// A pasted token of a Git identity of the backup and the id its value would have in the
/// credential store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityTokenRef {
    /// The identity's label.
    pub identity: String,
    /// The Git host the token is for.
    pub host: String,
    /// The credential-store id of the token.
    pub secret_id: String,
}

/// What the service knows about a staged file; the route adds the rest to make a [`crate::wire::RestorePreview`].
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
    /// The pasted tokens of its Git identities.
    pub token_refs: Vec<IdentityTokenRef>,
}

/// Reads and checks a backup file for a restore.
pub trait RestoreService: Send + Sync {
    /// Reads the uploaded archive into a private staging folder, checks it and compares it with
    /// this machine. Changes nothing that exists.
    fn preview(
        &self,
        upload: UploadStream,
    ) -> BoxFuture<'_, Result<RestoreFacts, RestoreServiceError>>;

    /// Confirms the restore of the staged copy `staging_id`: makes the safety copy of the present
    /// state first (the restore does not start when it cannot), then leaves the restore for the
    /// next start of Puddle and asks for that start.
    fn apply<'a>(
        &'a self,
        staging_id: &'a str,
        mode: RestoreMode,
    ) -> BoxFuture<'a, Result<RestoreApplied, RestoreServiceError>> {
        // A service with no restore has no use for the request.
        let _ = (staging_id, mode);
        unavailable()
    }

    /// What the last restore did; `None` when there is nothing to show.
    fn result(&self) -> BoxFuture<'_, Result<Option<RestoreResult>, RestoreServiceError>> {
        unavailable()
    }

    /// Forgets the last restore's result, once the user has seen it.
    fn dismiss_result(&self) -> BoxFuture<'_, Result<(), RestoreServiceError>> {
        unavailable()
    }

    /// The safety copies, newest first.
    fn safety_copies(&self) -> BoxFuture<'_, Result<Vec<SafetyCopy>, RestoreServiceError>> {
        unavailable()
    }

    /// Deletes one safety copy by its folder name; `NotStaged` when there is none by that name.
    fn delete_safety_copy<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), RestoreServiceError>> {
        // A service with no restore has no use for the name.
        let _ = name;
        unavailable()
    }
}

fn unavailable<'a, T: Send + 'a>() -> BoxFuture<'a, Result<T, RestoreServiceError>> {
    Box::pin(async {
        Err(RestoreServiceError::Unavailable(
            "restore is not available in this build".into(),
        ))
    })
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

#[cfg(any(test, feature = "testing"))]
/// An in-memory [`RestoreService`] for tests and the UI fixture: it accepts a body that starts with
/// the fake backup's first word, refuses anything else as a file that is not a backup, and
/// answers with the facts set with [`FakeRestore::set_facts`].
#[derive(Debug)]
pub struct FakeRestore {
    state: Mutex<FakeState>,
}

#[cfg(any(test, feature = "testing"))]
#[derive(Debug)]
struct FakeState {
    facts: RestoreFacts,
    previews: usize,
    last_upload_bytes: usize,
    applied: Vec<(String, RestoreMode)>,
    apply_failure: Option<String>,
    result: Option<RestoreResult>,
    copies: Vec<SafetyCopy>,
}

#[cfg(any(test, feature = "testing"))]
/// The first bytes a body must have for [`FakeRestore`] to accept it.
pub const FAKE_RESTORE_MAGIC: &[u8] = b"Puddle fake backup";

#[cfg(any(test, feature = "testing"))]
impl Default for FakeRestore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "testing"))]
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
                    token_refs: Vec::new(),
                },
                previews: 0,
                last_upload_bytes: 0,
                applied: Vec::new(),
                apply_failure: None,
                result: None,
                copies: Vec::new(),
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

    /// Makes every confirm from now on fail with `reason` (`None` makes it succeed again).
    pub fn fail_apply_with(&self, reason: Option<&str>) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .apply_failure = reason.map(str::to_owned);
    }

    /// Sets what the last restore did, as read after a restart.
    pub fn set_result(&self, result: Option<RestoreResult>) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .result = result;
    }

    /// Sets the safety copies the list shows.
    pub fn set_safety_copies(&self, copies: Vec<SafetyCopy>) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .copies = copies;
    }

    /// The confirms so far: the staging id and the mode of each.
    #[must_use]
    pub fn applied(&self) -> Vec<(String, RestoreMode)> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .applied
            .clone()
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

#[cfg(any(test, feature = "testing"))]
impl RestoreService for FakeRestore {
    fn apply<'a>(
        &'a self,
        staging_id: &'a str,
        mode: RestoreMode,
    ) -> BoxFuture<'a, Result<RestoreApplied, RestoreServiceError>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let answer = if state.facts.staging_id != staging_id {
            Err(RestoreServiceError::NotStaged(
                "this backup is no longer staged; choose the file again".into(),
            ))
        } else if let Some(reason) = &state.apply_failure {
            Err(RestoreServiceError::Failed(reason.clone()))
        } else {
            state.applied.push((staging_id.to_owned(), mode));
            Ok(RestoreApplied {
                safety_copy: "20231114T221320Z".into(),
                restart_required: true,
            })
        };
        Box::pin(async move { answer })
    }

    fn result(&self) -> BoxFuture<'_, Result<Option<RestoreResult>, RestoreServiceError>> {
        let result = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .result
            .clone();
        Box::pin(async move { Ok(result) })
    }

    fn dismiss_result(&self) -> BoxFuture<'_, Result<(), RestoreServiceError>> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .result = None;
        Box::pin(async { Ok(()) })
    }

    fn safety_copies(&self) -> BoxFuture<'_, Result<Vec<SafetyCopy>, RestoreServiceError>> {
        let copies = self
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .copies
            .clone();
        Box::pin(async move { Ok(copies) })
    }

    fn delete_safety_copy<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<(), RestoreServiceError>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let before = state.copies.len();
        state.copies.retain(|c| c.name != name);
        let answer = if state.copies.len() == before {
            Err(RestoreServiceError::NotStaged(format!(
                "no safety copy named {name}"
            )))
        } else {
            Ok(())
        };
        Box::pin(async move { answer })
    }

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
