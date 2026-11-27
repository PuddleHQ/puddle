// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore from a backup file, first step: the preview. The owner alone may ask (see
//! [`crate::authz::Action::Restore`]): a backup holds every user's rules and audit entries.

use std::collections::BTreeSet;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::StatusCode;
use futures_util::StreamExt as _;
use puddle_secrets::{SecretStore, StoredId};
use puddle_types::{WorkspaceId, WorkspaceStatus};

use crate::extract::{Json as JsonBody, Path as UrlPath};
use crate::routes::AppState;
use puddle_services::error::ApiError;
use puddle_services::error::ApiErrorBody;
use puddle_services::error::ErrorCode;
use puddle_services::restore::{IdentityTokenRef, RestoreFacts, RestoreService, SecretRef};
use puddle_services::wire::{
    RestoreApplied, RestoreApplyRequest, RestoreDisk, RestoreIdentityToken, RestorePreview,
    RestoreResultView, RestoreSecret, RestoreWorkspace, SafetyCopyList,
};

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
        (status = PAYLOAD_TOO_LARGE, description = "the file is larger than a backup can be; nothing is staged", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the file cannot be read faithfully; the message says why", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the file cannot be staged on this machine", body = ApiErrorBody)
    )
)]
pub(crate) async fn preview_restore(
    State(state): State<AppState>,
    body: Body,
) -> Result<Json<RestorePreview>, ApiError> {
    let facts = stage_upload(state.restore.as_ref(), body, MAX_UPLOAD_BYTES).await?;
    Ok(Json(enrich(&state, facts).await?))
}

/// Hands the body to the service as a stream that stops, and is answered with a 413, at the first
/// byte past `limit`. The router's `DefaultBodyLimit` does not reach a raw `Body`, so the count
/// is made here.
async fn stage_upload(
    restore: &dyn RestoreService,
    body: Body,
    limit: usize,
) -> Result<RestoreFacts, ApiError> {
    let exceeded = Arc::new(AtomicBool::new(false));
    let flag = exceeded.clone();
    let mut seen = 0usize;
    let upload = body
        .into_data_stream()
        .map(move |piece| {
            let piece = piece.map_err(io::Error::other)?;
            seen = seen.saturating_add(piece.len());
            if seen > limit {
                flag.store(true, Ordering::Release);
                return Err(io::Error::other(
                    "the upload is larger than a backup can be",
                ));
            }
            Ok(piece.to_vec())
        })
        .boxed();
    let staged = restore.preview(upload).await;
    if exceeded.load(Ordering::Acquire) {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::PayloadTooLarge,
            "the file is larger than a backup can be",
        ));
    }
    Ok(staged?)
}

/// Confirms a restore: the staged copy `staging_id` and the mode the user chose. Makes a safety copy
/// of the present state first and does not start when it cannot; then leaves the restore for the next
/// start of Puddle (which stops the workspaces the normal way) and asks for that start. Nothing
/// that exists changes until then.
#[utoipa::path(
    post,
    path = "/api/restore/apply",
    tag = "restore",
    request_body = RestoreApplyRequest,
    responses(
        (status = OK, description = "the restore waits for the next start", body = RestoreApplied),
        (status = FORBIDDEN, description = "only the owner may restore", body = ApiErrorBody),
        (status = NOT_FOUND, description = "the staged copy is gone; choose the file again", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the backup cannot be merged with this machine's data", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the safety copy could not be made, so the restore did not start", body = ApiErrorBody)
    )
)]
pub(crate) async fn apply_restore(
    State(state): State<AppState>,
    JsonBody(request): JsonBody<RestoreApplyRequest>,
) -> Result<Json<RestoreApplied>, ApiError> {
    Ok(Json(
        state
            .restore
            .apply(&request.staging_id, request.mode)
            .await?,
    ))
}

/// What the last restore did, after the restart: whether it was applied or undone, what changed or
/// why, and where the safety copy is. `result` is `null` when there is nothing to show.
#[utoipa::path(
    get,
    path = "/api/restore/result",
    tag = "restore",
    responses(
        (status = OK, description = "the last restore's result", body = RestoreResultView),
        (status = FORBIDDEN, description = "only the owner may restore", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_restore_result(
    State(state): State<AppState>,
) -> Result<Json<RestoreResultView>, ApiError> {
    Ok(Json(RestoreResultView {
        result: state.restore.result().await?,
    }))
}

/// Forgets the last restore's result, once the user has seen it.
#[utoipa::path(
    delete,
    path = "/api/restore/result",
    tag = "restore",
    responses(
        (status = NO_CONTENT, description = "forgotten"),
        (status = FORBIDDEN, description = "only the owner may restore", body = ApiErrorBody)
    )
)]
pub(crate) async fn dismiss_restore_result(
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    state.restore.dismiss_result().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The safety copies of the state each restore replaced, newest first, with their sizes. Puddle
/// never removes one; the user does, one at a time.
#[utoipa::path(
    get,
    path = "/api/safety-copies",
    tag = "restore",
    responses(
        (status = OK, description = "the safety copies", body = SafetyCopyList),
        (status = FORBIDDEN, description = "only the owner may restore", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_safety_copies(
    State(state): State<AppState>,
) -> Result<Json<SafetyCopyList>, ApiError> {
    Ok(Json(SafetyCopyList {
        copies: state.restore.safety_copies().await?,
    }))
}

/// Deletes one safety copy by its name. Restoring from it first is an ordinary restore of the file
/// it holds; once it is deleted that way back is gone.
#[utoipa::path(
    delete,
    path = "/api/safety-copies/{name}",
    tag = "restore",
    params(("name" = String, Path, description = "the safety copy's folder name")),
    responses(
        (status = NO_CONTENT, description = "deleted"),
        (status = FORBIDDEN, description = "only the owner may restore", body = ApiErrorBody),
        (status = NOT_FOUND, description = "no safety copy by that name", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_safety_copy(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> Result<StatusCode, ApiError> {
    state.restore.delete_safety_copy(&name).await?;
    Ok(StatusCode::NO_CONTENT)
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
    let (secrets_needing_value, identity_tokens_needing_value) =
        missing_values(state.secrets.clone(), facts.secret_refs, facts.token_refs).await?;
    Ok(RestorePreview {
        staging_id: facts.staging_id,
        format: facts.format,
        puddle_version: facts.puddle_version,
        schema_version: facts.schema_version,
        created_at_ms: facts.created_at_ms,
        parts: facts.parts,
        workspaces,
        secrets_needing_value,
        identity_tokens_needing_value,
        running_workspaces: u32::try_from(running).unwrap_or(u32::MAX),
    })
}

/// The secrets and identity tokens of the backup that have no value in the credential store here.
/// The store blocks (the OS keyring answers one id at a time), so the reads run off the async
/// workers.
async fn missing_values(
    store: Arc<dyn SecretStore>,
    secret_refs: Vec<SecretRef>,
    token_refs: Vec<IdentityTokenRef>,
) -> Result<(Vec<RestoreSecret>, Vec<RestoreIdentityToken>), ApiError> {
    let has_value = move |secret_id: &str| {
        StoredId::new(secret_id).is_ok_and(|id| matches!(store.get(&id), Ok(Some(_))))
    };
    tokio::task::spawn_blocking(move || {
        let secrets = secret_refs
            .into_iter()
            .filter(|secret| !has_value(&secret.secret_id))
            .map(|secret| RestoreSecret {
                workspace_id: secret.workspace_id,
                name: secret.name,
            })
            .collect();
        let tokens = token_refs
            .into_iter()
            .filter(|token| !has_value(&token.secret_id))
            .map(|token| RestoreIdentityToken {
                identity: token.identity,
                host: token.host,
            })
            .collect();
        (secrets, tokens)
    })
    .await
    .map_err(|error| ApiError::internal(&error))
}

#[cfg(test)]
mod tests;
