// SPDX-License-Identifier: AGPL-3.0-or-later
//! The log bundle download: Settings' "Export log bundle". The owner alone may ask (see
//! [`crate::authz::Action::ExportLogs`]): the logs name every user's workspaces and hosts.

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use puddle_log::{Bundle, write_bundle};

use crate::routes::AppState;
use puddle_services::error::{ApiError, ApiErrorBody, ErrorCode, blocking};

/// Makes a log bundle now and sends it as one file (`application/gzip`, named
/// `puddle-logs-<seconds>.tar.gz`): the versions, the platform, the settings and the recent log
/// files, with every secret cut out ([`puddle_log::redact`]). A `POST` so that no link, prefetch or
/// cache can start one.
#[utoipa::path(
    post,
    path = "/api/logs/bundle",
    tag = "logs",
    responses(
        (status = OK, description = "the log bundle", content_type = "application/gzip", body = Vec<u8>),
        (status = FORBIDDEN, description = "only the owner may export the logs", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the logs cannot be read", body = ApiErrorBody)
    )
)]
pub(crate) async fn create_log_bundle(State(state): State<AppState>) -> Result<Response, ApiError> {
    let seconds = state.clock.now_ms() / 1000;
    let repo = state.settings.clone();
    let bytes = blocking(move || {
        let settings = repo.load_global().ok().flatten();
        let folder = puddle_log::log_folder_in_use().unwrap_or_default();
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok();
        let versions = [("puddle".to_owned(), puddle_types::VERSION.to_owned())];
        let platform = format!("{} {}", std::env::consts::OS, std::env::consts::ARCH);
        let mut bytes = Vec::new();
        write_bundle(
            &Bundle {
                versions: &versions,
                platform: &platform,
                log_level: puddle_log::current_level(),
                settings,
                log_folder: &folder,
                home: home.as_deref(),
                generated: seconds,
            },
            &mut bytes,
        )
        .map_err(|e| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                ErrorCode::Unavailable,
                e.to_string(),
            )
        })?;
        Ok(bytes)
    })
    .await?;
    let disposition = format!("attachment; filename=\"puddle-logs-{seconds}.tar.gz\"");
    let mut response = Response::new(axum::body::Body::from(bytes));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    // A file name made of digits and fixed letters is always a valid header value.
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response.into_response())
}
