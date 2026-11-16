// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace's managed tools: what its mise files ask for, and edits of the root `mise.toml`
//! through mise's own commands. The routes only translate; [`crate::WorkspaceService`] does the
//! work. Everything in an answer that came from the guest is untrusted text: escape it.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_types::{ToolName, ToolVersion, ToolsFileHash};
use serde::Deserialize;

use crate::ApiErrorBody;
use crate::error::ApiError;
use crate::extract::{Path, Query};
use crate::routes::AppState;
use crate::routes::workspaces::workspace_id;
use crate::wire::{ToolChangeRequest, ToolRegistry, ToolVersions, ToolsListing};

fn invalid<E: std::fmt::Display>(e: E) -> ApiError {
    ApiError::invalid(e.to_string())
}

/// `?sha256=` of a removal.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HashQuery {
    sha256: String,
}

/// The tools every mise file of the workspace asks for, as mise lists them in the guest
/// (`mise ls`), with whether each is installed. `files` has the root `mise.toml` (the only one
/// the API edits) and the repositories' own files, read-only. A stopped workspace answers its
/// last listing of this run with `running` false and `problem` saying to start it; tools off for
/// the workspace, or a mise that failed, is `problem` too, not an error. Send `sha256` back with a
/// change.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/tools",
    tag = "tools",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the tools", body = ToolsListing),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_tools(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ToolsListing>, ApiError> {
    Ok(Json(state.workspaces.tools(&workspace_id(&id)?).await?))
}

/// Sets a tool's version in the root `mise.toml` with `mise config set`: every other line, comments
/// included, stays as it was, and only plain `[tools]` strings are written. Puddle then installs
/// in the background (`workspace_tools` events). Answers the listing that follows.
#[utoipa::path(
    put,
    path = "/api/workspaces/{id}/tools/{name}",
    tag = "tools",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("name" = String, Path, description = "the tool's name")
    ),
    request_body = ToolChangeRequest,
    responses(
        (status = OK, description = "the tools after the change", body = ToolsListing),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running, tools are off for it, or `mise.toml` is not the file the user looked at (its SHA-256 differs): list again", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a bad name, version or hash", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "mise could not make the change; the message is its last error line", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_tool(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    crate::extract::Json(body): crate::extract::Json<ToolChangeRequest>,
) -> Result<Json<ToolsListing>, ApiError> {
    let id = workspace_id(&id)?;
    let name = ToolName::new(&name).map_err(invalid)?;
    let version = ToolVersion::new(&body.version).map_err(invalid)?;
    let hash = ToolsFileHash::new(&body.sha256).map_err(invalid)?;
    Ok(Json(
        state
            .workspaces
            .set_tool(&id, &name, &version, &hash)
            .await?,
    ))
}

/// Takes a tool out of the root `mise.toml` with `mise unuse --no-prune`: the other lines stay,
/// and the installed copy stays on the disk. Answers the listing that follows.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/tools/{name}",
    tag = "tools",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("name" = String, Path, description = "the tool's name"),
        ("sha256" = String, Query, description = "the `sha256` of the listing the user looked at")
    ),
    responses(
        (status = OK, description = "the tools after the change", body = ToolsListing),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running, tools are off for it, or the file changed since the listing", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a bad name or hash", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "mise could not make the change; the message is its last error line", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_tool(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    Query(query): Query<HashQuery>,
) -> Result<Json<ToolsListing>, ApiError> {
    let id = workspace_id(&id)?;
    let name = ToolName::new(&name).map_err(invalid)?;
    let hash = ToolsFileHash::new(&query.sha256).map_err(invalid)?;
    Ok(Json(state.workspaces.remove_tool(&id, &name, &hash).await?))
}

/// The tools mise knows by name (`mise registry`, about a thousand), for the Add tool dialog's
/// search. Cached per mise version. Needs a running workspace with tools on.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/tools/registry",
    tag = "tools",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the registry; empty with `problem` when mise could not say", body = ToolRegistry),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running or tools are off for it", body = ApiErrorBody)
    )
)]
pub(crate) async fn tool_registry(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ToolRegistry>, ApiError> {
    Ok(Json(
        state.workspaces.tool_registry(&workspace_id(&id)?).await?,
    ))
}

/// The newest 200 versions mise lists for a tool (`mise ls-remote`), newest first. With the
/// version host denied the answer is an empty list and `reason`, so the UI can offer a text box.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/tools/{name}/versions",
    tag = "tools",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("name" = String, Path, description = "the tool's name")
    ),
    responses(
        (status = OK, description = "the versions; empty with `reason` when mise could not list them", body = ToolVersions),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running or tools are off for it", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a bad name", body = ApiErrorBody)
    )
)]
pub(crate) async fn tool_versions(
    State(state): State<AppState>,
    Path((id, name)): Path<(String, String)>,
) -> Result<Json<ToolVersions>, ApiError> {
    let id = workspace_id(&id)?;
    let name = ToolName::new(&name).map_err(invalid)?;
    Ok(Json(state.workspaces.tool_versions(&id, &name).await?))
}

/// Installs what the workspace's mise files ask for, in the background: the retry of a failed
/// install. Progress and the end come as `workspace_tools` events.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/tools/install",
    tag = "tools",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = ACCEPTED, description = "the install started"),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running or tools are off for it", body = ApiErrorBody)
    )
)]
pub(crate) async fn install_tools(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.workspaces.install_tools(&workspace_id(&id)?).await?;
    Ok(StatusCode::ACCEPTED)
}
