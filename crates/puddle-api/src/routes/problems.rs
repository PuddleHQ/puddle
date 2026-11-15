// SPDX-License-Identifier: AGPL-3.0-or-later
//! The background problems that stand: things Puddle does on its own that failed.

use axum::Json;
use axum::extract::State;

use puddle_store::Role;

use crate::authz::{self, Principal};
use crate::error::ApiError;
use crate::routes::AppState;
use crate::wire::ProblemList;

/// What Puddle did on its own that failed and has not been put right: a sweep that could not
/// write, a clean-up that left something behind, a step at start-up that did not finish. Each
/// entry names what went wrong and what to do; one ends by itself when its cause does. A client
/// refetches on the `problems_changed` event.
#[utoipa::path(
    get,
    path = "/api/problems",
    tag = "service",
    responses((status = OK, description = "the problems that stand now", body = ProblemList))
)]
pub(crate) async fn list_problems(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<ProblemList>, ApiError> {
    let owners = authz::owners(&state).await?;
    let mut problems = state.problems.list();
    // A workspace's problems are its user's; Puddle's own are the owner's.
    problems.retain(|p| match &p.workspace {
        Some(workspace) => owners.is(workspace, principal.user),
        None => principal.role == Role::Owner,
    });
    Ok(Json(ProblemList { problems }))
}
