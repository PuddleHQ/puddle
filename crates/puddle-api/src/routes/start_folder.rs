// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's start folder: where new terminals land and what the editor opens.

use axum::Json;
use axum::extract::State;
use puddle_types::{Event, EventSink as _};

use crate::ApiErrorBody;
use crate::error::ApiError;
use crate::extract::Path;
use crate::routes::AppState;
use crate::routes::workspaces::workspace_id;
use crate::wire::{StartFolderRequest, StartFolderSet};

/// Sets the start folder: a path under the volume, or `null` for the volume root. The folder may
/// not exist yet. A running workspace's new terminals follow at once, and the answer says whether
/// the folder exists there. A folder that is missing opens the volume root and shows a problem
/// until it exists or the setting changes. A path that breaks the rules is refused with 422 and
/// the reason; a workspace that is being created or deleted answers 409.
#[utoipa::path(
    put,
    path = "/api/workspaces/{id}/start-folder",
    tag = "start-folder",
    params(("id" = String, Path, description = "workspace id")),
    request_body = StartFolderRequest,
    responses(
        (status = OK, description = "the start folder now", body = StartFolderSet),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is being created or deleted", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the path breaks the start folder's rules", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_start_folder(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<StartFolderRequest>,
) -> Result<Json<StartFolderSet>, ApiError> {
    let id = workspace_id(&id)?;
    let start_folder = body.into_start_folder()?;
    let change = state.workspaces.set_start_folder(&id, start_folder).await?;
    state.events.emit(Event::WorkspaceRepositoriesChanged {
        workspace: change.record.name.clone(),
    });
    Ok(Json(change.into()))
}
