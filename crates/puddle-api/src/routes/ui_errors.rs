// SPDX-License-Identifier: AGPL-3.0-or-later
//! Errors the app's pages hit that no code expected: a page reports one, the host's log gets it
//! scrubbed, and Settings lists the recent ones.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::ApiErrorBody;
use crate::authz::Principal;
use crate::error::ApiError;
use crate::routes::AppState;
use crate::ui_errors::{Outcome, PER_WINDOW, WINDOW_MS};
use crate::wire::{UiErrorList, UiErrorReport};

/// A page's report of an error no code expected: one that showed the error page, one thrown
/// outside the page's own handling, or a promise nothing handled. The server takes the user
/// info, query and fragment out of every URL and removes what looks like a token, bounds each
/// field, writes the error to its log and keeps it for `GET /api/ui-errors`. A user's pages may
/// report 10 errors a minute; past that the server refuses with `Retry-After`.
#[utoipa::path(
    post,
    path = "/api/ui-errors",
    tag = "service",
    request_body = UiErrorReport,
    responses(
        (status = NO_CONTENT, description = "logged and kept"),
        (status = TOO_MANY_REQUESTS, description = "too many reports this minute; `Retry-After` says when to send again", body = ApiErrorBody)
    )
)]
pub(crate) async fn report_ui_error(
    State(state): State<AppState>,
    principal: Principal,
    crate::extract::Json(report): crate::extract::Json<UiErrorReport>,
) -> Result<StatusCode, ApiError> {
    match state
        .ui_errors
        .report(principal.user, &report, state.clock.now_ms())
    {
        Outcome::Kept(error) => {
            tracing::warn!(
                kind = ?error.kind,
                route = %error.route.as_deref().unwrap_or("-"),
                build = %error.build.as_deref().unwrap_or("-"),
                error = ?error.message,
                stack = ?error.stack.as_deref().unwrap_or(""),
                "a page hit an unexpected error"
            );
            Ok(StatusCode::NO_CONTENT)
        }
        Outcome::Refused {
            retry_after_s,
            first,
        } => {
            if first {
                tracing::warn!(
                    limit = PER_WINDOW,
                    window_s = WINDOW_MS / 1000,
                    "pages reported too many errors; the next ones are dropped until the rate allows"
                );
            }
            Err(ApiError::too_many(
                format!("too many error reports; send again in {retry_after_s} seconds"),
                retry_after_s,
            ))
        }
    }
}

/// The errors the caller's pages reported since Puddle started, newest first (at most 50 kept,
/// for all users together), and how many reports were refused for coming too fast.
#[utoipa::path(
    get,
    path = "/api/ui-errors",
    tag = "service",
    responses((status = OK, description = "the recent errors", body = UiErrorList))
)]
pub(crate) async fn list_ui_errors(
    State(state): State<AppState>,
    principal: Principal,
) -> Json<UiErrorList> {
    Json(state.ui_errors.list(principal.user))
}
