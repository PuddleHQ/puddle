// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's ports: what listens in it, the forwards to this computer, and pinned forwards.
//!
//! A forward is a listener on `127.0.0.1` of this computer that leads to one port of the
//! workspace. It is opened only while the workspace runs. A pinned one is kept and comes back at
//! every start.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_types::WorkspaceName;

use crate::ApiErrorBody;
use crate::error::ApiError;
use crate::extract::{Json as BodyJson, Path};
use crate::routes::AppState;
use crate::routes::identities::workspace;
use crate::wire::{PortEntry, PortForwardRequest, WorkspacePorts};

/// The workspace's ports: the ones a process in it listens on (as the workspace reports them),
/// the ones forwarded to this computer and the pinned ones. A stopped workspace lists its pinned
/// forwards only.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/ports",
    tag = "ports",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the workspace's ports", body = WorkspacePorts),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "ports are not available in this build", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_ports(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<WorkspacePorts>, ApiError> {
    let name: WorkspaceName = workspace(&state, &id).await?;
    Ok(Json(state.ports.list(&name).await?))
}

/// Forwards a port of the running workspace to a port of this computer's loopback, and keeps it
/// (`pinned`) or not. Asking again for what is already so changes nothing. The port of this
/// computer is bound by Puddle and only on `127.0.0.1`; if something else has it, the refusal
/// says who.
#[utoipa::path(
    put,
    path = "/api/workspaces/{id}/ports/{port}",
    tag = "ports",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("port" = u16, Path, description = "the port in the workspace")
    ),
    request_body = PortForwardRequest,
    responses(
        (status = OK, description = "the port now", body = PortEntry),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = CONFLICT, description = "the workspace is not running, or the port of this computer is in use (the message says by whom)", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a port that is not 1 to 65535", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "ports are not available in this build", body = ApiErrorBody)
    )
)]
pub(crate) async fn forward_port(
    State(state): State<AppState>,
    Path((id, port)): Path<(String, u16)>,
    BodyJson(body): BodyJson<PortForwardRequest>,
) -> Result<Json<PortEntry>, ApiError> {
    let name = workspace(&state, &id).await?;
    Ok(Json(
        state
            .ports
            .forward(&name, port, body.host_port, body.pinned)
            .await?,
    ))
}

/// Stops the forward of a port of the workspace and unpins it.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/ports/{port}",
    tag = "ports",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("port" = u16, Path, description = "the port in the workspace")
    ),
    responses(
        (status = NO_CONTENT, description = "stopped and unpinned"),
        (status = NOT_FOUND, description = "no such workspace, or that port is neither forwarded nor pinned", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "ports are not available in this build", body = ApiErrorBody)
    )
)]
pub(crate) async fn release_port(
    State(state): State<AppState>,
    Path((id, port)): Path<(String, u16)>,
) -> Result<StatusCode, ApiError> {
    let name = workspace(&state, &id).await?;
    state.ports.release(&name, port).await?;
    Ok(StatusCode::NO_CONTENT)
}
