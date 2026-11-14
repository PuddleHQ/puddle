// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workspace disks the workspace list does not name: listing them and restoring one.
//!
//! Puddle keeps such a disk (a lost or outdated workspace list is the usual cause) and never
//! removes it. A restore adds the list entry back; there is no route that deletes a disk.

use crate::authz::Principal;
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_types::WorkspaceId;

use crate::ApiErrorBody;
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::routes::workspaces::{StoreGitSetup, view};
use crate::wire::{OrphanVolumeList, OrphanVolumeView, RestoreOrphanRequest, Workspace};
use crate::workspaces::{GitSetup as _, RepoUrl};

/// The workspace disks Puddle keeps that no workspace in its list claims, read fresh from the
/// runtime on every call. Puddle cannot tell from outside whether such a disk holds any files.
/// A client refetches on `problems_changed` and when the workspace list changes.
#[utoipa::path(
    get,
    path = "/api/orphan-volumes",
    tag = "orphan-volumes",
    responses(
        (status = OK, description = "the unclaimed workspace disks", body = OrphanVolumeList),
        (status = SERVICE_UNAVAILABLE, description = "the runtime cannot list its disks", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_orphan_volumes(
    State(state): State<AppState>,
) -> Result<Json<OrphanVolumeList>, ApiError> {
    let ids = state.workspaces.orphan_volumes().await?;
    Ok(Json(OrphanVolumeList {
        volumes: ids
            .into_iter()
            .map(|id| OrphanVolumeView {
                volume: id.volume_name().to_string(),
                id: id.to_string(),
            })
            .collect(),
    }))
}

/// Adds a list entry for an unclaimed workspace disk, named after the disk. Its repositories are
/// the checkouts on the disk that have a readable remote address (read in a short-lived
/// maintenance sandbox with no route out); when there is none, `repository` is used. Nothing is
/// written to the disk and an existing list entry is never replaced. The workspace is stopped:
/// start it to use it.
#[utoipa::path(
    post,
    path = "/api/orphan-volumes/{id}/restore",
    tag = "orphan-volumes",
    params(("id" = String, Path, description = "the disk's workspace id")),
    request_body = RestoreOrphanRequest,
    responses(
        (status = CREATED, description = "the workspace, now in the list", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace disk", body = ApiErrorBody),
        (status = CONFLICT, description = "the list already names the workspace, or something holds the disk", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "no address could be read from the disk and none was given, or the address is refused", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the list cannot be saved, or the disk cannot be read", body = ApiErrorBody)
    )
)]
pub(crate) async fn restore_orphan_volume(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<RestoreOrphanRequest>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let id = WorkspaceId::new(&id)
        .map_err(|_| ApiError::not_found(format!("no workspace disk {id:?}")))?;
    let repository = body
        .repository
        .as_deref()
        .map(|url| RepoUrl::parse(url).map_err(ApiError::invalid))
        .transpose()?;
    let store = state.store.clone();
    let restored = id.clone();
    blocking(move || {
        // The disk of a workspace a user had stays theirs; any other becomes the restorer's (only
        // the owner restores disks). Recorded first, so the workspace is never listed as nobody's.
        let owner = store.workspace_owner(&restored)?.unwrap_or(principal.user);
        Ok(StoreGitSetup::new(store).claim(&restored, owner)?)
    })
    .await?;
    let git = StoreGitSetup::new(state.store.clone());
    let record = state
        .workspaces
        .restore_orphan(&id, repository, &git)
        .await?;
    crate::system_managed::refresh(&state).await;
    Ok((
        StatusCode::CREATED,
        Json(view(&state, record).await.with_identities(git.take())),
    ))
}
