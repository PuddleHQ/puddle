// SPDX-License-Identifier: AGPL-3.0-or-later
//! Clients tell the server whether they are in front and when they last saw input, so a new
//! request is announced on one of them and not on all.

use axum::extract::State;
use axum::http::StatusCode;

use crate::ApiErrorBody;
use crate::authz::Principal;
use crate::error::ApiError;
use crate::routes::AppState;
use crate::wire::PresenceReport;

/// A client's report of itself. Clients send it every 30 seconds and when something in it changes.
/// A client that stops reporting is no longer picked after about a minute.
#[utoipa::path(
    post,
    path = "/api/presence",
    tag = "presence",
    request_body = PresenceReport,
    responses(
        (status = NO_CONTENT, description = "noted"),
        (status = BAD_REQUEST, description = "the client id is not 1 to 64 letters, digits, `-` or `_`", body = ApiErrorBody)
    )
)]
pub(crate) async fn report(
    State(state): State<AppState>,
    principal: Principal,
    crate::extract::Json(report): crate::extract::Json<PresenceReport>,
) -> Result<StatusCode, ApiError> {
    if !crate::presence::valid_client_id(&report.client) {
        return Err(ApiError::bad_request(
            "the client id must be 1 to 64 letters, digits, `-` or `_`",
        ));
    }
    state
        .presence
        .report(principal.user, &report, state.clock.now_ms());
    Ok(StatusCode::NO_CONTENT)
}
