// SPDX-License-Identifier: GPL-3.0-or-later
//! The logins Puddle kept for a workspace: listing them and forgetting one.
//!
//! A login made inside a workspace keeps its real tokens in the host's credential store (login
//! capture). With capture off the workspace's stand-ins are no longer swapped, and the
//! notice that says so stays until the kept tokens are gone. The list shows what is kept, so a
//! login can be forgotten after its notice was dismissed; forgetting is the way out.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_types::WorkspaceId;

use crate::ApiErrorBody;
use crate::error::ApiError;
use crate::extract::Path;
use crate::routes::AppState;
use crate::wire::{KeptLoginList, KeptLoginView};

/// The services Puddle keeps a login for in a workspace, with when each was first noted. No token
/// or other secret is in the answer. It comes from Puddle's own record of what is kept, so a
/// locked credential store does not stall it; only a workspace with no record yet asks the store,
/// and a store that does not answer shows as `store_readable: false`.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/logins",
    tag = "logins",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the kept logins", body = KeptLoginList),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "this Puddle keeps no logins", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_logins(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<KeptLoginList>, ApiError> {
    let id =
        WorkspaceId::new(&id).map_err(|_| ApiError::not_found(format!("no workspace {id:?}")))?;
    let kept = state.workspaces.kept_logins(&id).await?;
    Ok(Json(KeptLoginList {
        logins: kept
            .logins
            .into_iter()
            .map(|login| KeptLoginView {
                service: login.service,
                name: login.name,
                since: login.since,
            })
            .collect(),
        store_readable: kept.store_readable,
    }))
}

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
