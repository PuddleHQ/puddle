// SPDX-License-Identifier: GPL-3.0-or-later
//! The logins Puddle kept for a workspace: forgetting one.
//!
//! A login made inside a workspace keeps its real tokens in the host's credential store (login
//! capture). With capture off the workspace's stand-ins are no longer swapped, and the
//! notice that says so stays until the kept tokens are gone. Forgetting is the way out.

use axum::extract::State;
use axum::http::StatusCode;
use puddle_types::WorkspaceId;

use crate::ApiErrorBody;
use crate::error::ApiError;
use crate::extract::Path;
use crate::routes::AppState;

/// Forgets the login Puddle kept for a service (`claude`, `github`) in a workspace: the real
/// tokens are deleted from the credential store. The workspace's own copy is only a stand-in,
/// which stops working in a running workspace; the tool asks for a new sign-in. Forgetting a
/// service that has nothing kept is fine.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/logins/{service}",
    tag = "logins",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("service" = String, Path, description = "the service: `claude` or `github`")
    ),
    responses(
        (status = NO_CONTENT, description = "forgotten"),
        (status = NOT_FOUND, description = "no such workspace, or Puddle has no such service", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the credential store refused a deletion; what was deleted stays deleted", body = ApiErrorBody)
    )
)]
pub(crate) async fn forget_login(
    State(state): State<AppState>,
    Path((id, service)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let id =
        WorkspaceId::new(&id).map_err(|_| ApiError::not_found(format!("no workspace {id:?}")))?;
    state.workspaces.forget_login(&id, &service).await?;
    Ok(StatusCode::NO_CONTENT)
}
