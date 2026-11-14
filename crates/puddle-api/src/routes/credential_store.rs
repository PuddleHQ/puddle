// SPDX-License-Identifier: AGPL-3.0-or-later
//! Checking the credential store again, so the first-run banner's Retry works without a restart.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;

use crate::ApiErrorBody;
use crate::error::{ApiError, ErrorCode};
use crate::routes::AppState;
use crate::wire::CredentialStoreRetry;

/// Connects to the credential store again and checks that it takes a write. The
/// `credential-store` problem in `GET /api/problems` is raised or cleared to match, and
/// `problems_changed` says so.
#[utoipa::path(
    post,
    path = "/api/credential-store/retry",
    tag = "first-run",
    responses(
        (status = OK, description = "whether the store takes a write now", body = CredentialStoreRetry),
        (status = SERVICE_UNAVAILABLE, description = "this build has no credential store check", body = ApiErrorBody)
    )
)]
pub(crate) async fn retry_credential_store(
    State(state): State<AppState>,
) -> Result<Json<CredentialStoreRetry>, ApiError> {
    let service = state.credential_store;
    let answer = tokio::task::spawn_blocking(move || service.retry())
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                ErrorCode::Internal,
                "the credential store check stopped unexpectedly",
            )
        })?;
    match answer {
        Some(working) => Ok(Json(CredentialStoreRetry { working })),
        None => Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "the credential store check is not available in this build",
        )),
    }
}
