// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's checkouts, and adding, managing and removing the repositories on its list. The
//! routes only translate; [`crate::WorkspaceService`] does the work.

use std::collections::BTreeSet;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_store::IdentityId;
use puddle_types::CheckoutDir;

use crate::ApiErrorBody;
use crate::checkouts::AddRepository;
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::routes::workspaces::{StoreGitSetup, git_request, view, workspace_id};
use crate::wire::{AddRepositoryRequest, Checkouts, Workspace};

/// The directory in a path. A string that can't be a checkout directory names no entry.
fn entry_directory(raw: &str) -> Result<CheckoutDir, ApiError> {
    CheckoutDir::new(raw)
        .map_err(|_| ApiError::not_found(format!("no repository {raw:?} on the list")))
}

/// The checkouts directly under the workspace's volume root, each with the list entry that has
/// its directory. A running workspace lists them now (calls at the same moment share one
/// listing, which takes at most 10 seconds); a stopped one answers the last listing this run
/// made, or says to start it. A listing that cannot be made or read is `problem`, not an error.
/// Every string in it came from the guest: escape it.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/checkouts",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the checkouts", body = Checkouts),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_checkouts(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Checkouts>, ApiError> {
    let checkouts = state.workspaces.checkouts(&workspace_id(&id)?).await?;
    Ok(Json(checkouts.into()))
}

/// Adds a repository to a running workspace's list. With `kind` `clone` it clones a new one: the
/// directory must not exist, Git settings are set up first, and the answer is `202` with the
/// workspace busy `cloning` (stop and delete answer `409` until it ends; rename works), followed
/// by `workspace_progress`. A failed clone leaves the entry with a background problem. With
/// `kind` `existing` it manages a checkout already on the volume: Puddle reads its remote again
/// in the guest, adds the entry and its Git settings, changes nothing in the checkout and answers
/// `200` with the workspace.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/repositories",
    tag = "workspaces",
    params(("id" = String, Path, description = "workspace id")),
    request_body = AddRepositoryRequest,
    responses(
        (status = OK, description = "the checkout is on the list now (kind `existing`)", body = Workspace),
        (status = ACCEPTED, description = "the clone started (kind `clone`)", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running or is busy, the list is full, the repository or directory is already listed, the directory exists (clone) or is no checkout or has no usable remote (existing), or the identities collide (`identity_collision`)", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a bad address, directory or identity", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the list could not be saved, nothing changed", body = ApiErrorBody)
    )
)]
pub(crate) async fn add_repository(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<AddRepositoryRequest>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let id = workspace_id(&id)?;
    let store = state.store.clone();
    let known: BTreeSet<IdentityId> =
        blocking(move || Ok(store.identities()?.into_iter().map(|i| i.id).collect())).await?;
    let add = body.into_add(|identity| known.contains(&identity))?;
    let record = state.workspaces.get(&id).await?;
    let status = match &add {
        AddRepository::Clone(new) => {
            let requests: Vec<_> = git_request(new).into_iter().collect();
            let (store, name) = (state.store.clone(), record.name.clone());
            blocking(move || Ok(store.check_repository_git(&name, &requests)?)).await?;
            StatusCode::ACCEPTED
        }
        AddRepository::Existing { .. } => StatusCode::OK,
    };
    let git = StoreGitSetup::new(state.store.clone());
    let record = state.workspaces.add_repository(&id, add, &git).await?;
    Ok((
        status,
        Json(view(&state, record).await.with_identities(git.take())),
    ))
}

/// Clones an entry whose directory is not on the volume again, as adding does. Answers `202`
/// with the workspace busy `cloning`.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/repositories/{directory}/clone",
    tag = "workspaces",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("directory" = String, Path, description = "the entry's directory")
    ),
    responses(
        (status = ACCEPTED, description = "the clone started", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace or entry", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running or is busy, or the directory is there", body = ApiErrorBody)
    )
)]
pub(crate) async fn clone_again(
    State(state): State<AppState>,
    Path((id, directory)): Path<(String, String)>,
) -> Result<(StatusCode, Json<Workspace>), ApiError> {
    let (id, directory) = (workspace_id(&id)?, entry_directory(&directory)?);
    let git = StoreGitSetup::new(state.store.clone());
    let record = state.workspaces.clone_again(&id, &directory, &git).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(view(&state, record).await.with_identities(git.take())),
    ))
}

/// Takes an entry off the list and nothing else: the checkout and its files stay (and show as a
/// checkout without an entry), and so do the repository table row and the identities. Allowed in
/// every state but creating and deleting. No call deletes a checkout.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/repositories/{directory}",
    tag = "workspaces",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("directory" = String, Path, description = "the entry's directory")
    ),
    responses(
        (status = OK, description = "the workspace without the entry", body = Workspace),
        (status = NOT_FOUND, description = "no such workspace or entry", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is being created or deleted", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the list could not be saved, nothing changed", body = ApiErrorBody)
    )
)]
pub(crate) async fn remove_repository(
    State(state): State<AppState>,
    Path((id, directory)): Path<(String, String)>,
) -> Result<Json<Workspace>, ApiError> {
    let (id, directory) = (workspace_id(&id)?, entry_directory(&directory)?);
    let record = state.workspaces.remove_repository(&id, &directory).await?;
    Ok(Json(view(&state, record).await))
}
