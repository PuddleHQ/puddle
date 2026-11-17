// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore from a backup file, first step: the preview. The owner alone may ask (see
//! [`crate::authz::Action::Restore`]): a backup holds every user's rules and audit entries.

use std::collections::BTreeSet;
use std::io;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use futures_util::StreamExt as _;
use puddle_secrets::StoredId;
use puddle_types::{WorkspaceId, WorkspaceStatus};

use crate::ApiErrorBody;
use crate::error::ApiError;
use crate::restore::{RestoreFacts, RestoreServiceError};
use crate::routes::AppState;
use crate::wire::{RestoreDisk, RestorePreview, RestoreSecret, RestoreWorkspace};

/// The most an upload may be: the unpacked limit, since a gzip archive that unpacks to no more
/// cannot be much bigger, and a larger body is refused before it fills the disk.
pub(crate) const MAX_UPLOAD_BYTES: usize = 4 * 1024 * 1024 * 1024 + 64 * 1024;

/// Reads the uploaded backup (the file as the request body: a `.tar.gz` made by "Back up now") into
/// a private staging area, checks it and answers what restoring it would do on this machine: counts
/// of what would be added, replaced, kept or dropped, which restored workspaces have a disk here,
/// which secrets need a value, and how many workspaces run. Nothing that exists changes. A file
/// that cannot be read faithfully is refused with the reason (a file that is not a gzip archive, an
/// entry outside the backup's three names or its manifest, a path or link that points out of it,
/// more than 4 GiB unpacked, no database, a failed integrity check, a newer schema, an unknown
/// format) and leaves nothing staged. A staged copy that is not applied is removed at the next
/// start and after 24 hours.
#[utoipa::path(
    post,
    path = "/api/restore/preview",
    tag = "restore",
    request_body(content = Vec<u8>, content_type = "application/gzip", description = "the backup file"),
    responses(
        (status = OK, description = "what a restore of this file would do", body = RestorePreview),
        (status = FORBIDDEN, description = "only the owner may restore", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the file cannot be read faithfully; the message says why", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the file cannot be staged on this machine", body = ApiErrorBody)
    )
)]
pub(crate) async fn preview_restore(
    State(state): State<AppState>,
    body: Body,
) -> Result<Json<RestorePreview>, ApiError> {
    let upload = body
        .into_data_stream()
        .map(|piece| piece.map(|b| b.to_vec()).map_err(io::Error::other))
        .boxed();
    let facts = state.restore.preview(upload).await?;
    Ok(Json(enrich(&state, facts).await?))
}

/// Adds what only the API knows: which restored workspaces have a disk here, which secrets have no
/// value here, and how many workspaces run.
async fn enrich(state: &AppState, facts: RestoreFacts) -> Result<RestorePreview, ApiError> {
    let listed = state.workspaces.list().await?;
    let orphans: BTreeSet<WorkspaceId> = state
        .workspaces
        .orphan_volumes()
        .await
        .unwrap_or_default()
        .into_iter()
        .collect();
    let here: BTreeSet<&WorkspaceId> = listed.iter().map(|w| &w.id).collect();
    let running = listed
        .iter()
        .filter(|w| {
            matches!(
                w.status,
                WorkspaceStatus::Running | WorkspaceStatus::Starting
            )
        })
        .count();
    let workspaces = facts
        .workspaces
        .into_iter()
        .map(|(id, name)| {
            let disk = match WorkspaceId::new(&id) {
                Ok(parsed) if here.contains(&parsed) || orphans.contains(&parsed) => {
                    RestoreDisk::Present
                }
                _ => RestoreDisk::Missing,
            };
            RestoreWorkspace { id, name, disk }
        })
        .collect();
    let mut secrets_needing_value = Vec::new();
    for secret in facts.secret_refs {
        let has_value = match StoredId::new(&secret.secret_id) {
            Ok(id) => matches!(state.secrets.get(&id), Ok(Some(_))),
            Err(_) => false,
        };
        if !has_value {
            secrets_needing_value.push(RestoreSecret {
                workspace_id: secret.workspace_id,
                name: secret.name,
            });
        }
    }
    Ok(RestorePreview {
        staging_id: facts.staging_id,
        format: facts.format,
        puddle_version: facts.puddle_version,
        schema_version: facts.schema_version,
        created_at_ms: facts.created_at_ms,
        parts: facts.parts,
        workspaces,
        secrets_needing_value,
        running_workspaces: u32::try_from(running).unwrap_or(u32::MAX),
    })
}

impl From<RestoreServiceError> for ApiError {
    fn from(err: RestoreServiceError) -> Self {
        match err {
            RestoreServiceError::Refused(m) => Self::invalid(m),
            RestoreServiceError::NotStaged(m) => Self::not_found(m),
            RestoreServiceError::Unavailable(m) | RestoreServiceError::Failed(m) => Self::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                crate::error::ErrorCode::Unavailable,
                m,
            ),
        }
    }
}

#[cfg(test)]
mod tests;
