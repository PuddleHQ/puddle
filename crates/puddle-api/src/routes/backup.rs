// SPDX-License-Identifier: AGPL-3.0-or-later
//! The backup download: Settings' "Back up now". The owner alone may ask (see
//! [`crate::authz::Action::BackUp`]): the file holds every user's rules and audit entries.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::stream;
use tokio::io::{AsyncRead, AsyncReadExt as _};

use crate::ApiErrorBody;
use crate::backup::archive_file_name;
use crate::error::ApiError;
use crate::routes::AppState;

/// The size of one piece of the file as it is sent.
const CHUNK_BYTES: usize = 64 * 1024;

/// Makes a backup now and sends it as one file (`application/gzip`, named
/// `puddle-backup-<UTC time>.tar.gz`). It holds the rules, settings, workspace list, identities,
/// users and audit log as of one moment, and never a secret value, a password hash or a workspace
/// disk. The file is sent as it is read, so its size does not set a memory limit; a failure while
/// it is moving ends the connection, so a client that sees the whole body has a complete file.
/// A `POST` so that no link, prefetch or cache can start one.
#[utoipa::path(
    post,
    path = "/api/backup",
    tag = "backup",
    responses(
        (status = OK, description = "the backup file", content_type = "application/gzip", body = Vec<u8>),
        (status = FORBIDDEN, description = "only the owner may make a backup", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the data cannot be read or the file cannot be written", body = ApiErrorBody)
    )
)]
pub(crate) async fn create_backup(State(state): State<AppState>) -> Result<Response, ApiError> {
    let file_name = archive_file_name(state.clock.now_ms());
    let archive = state.backup.create().await?;
    let size = archive.size();
    let disposition = format!("attachment; filename=\"{file_name}\"");
    let mut response = Response::new(Body::from_stream(chunks(archive.into_reader())));
    *response.status_mut() = StatusCode::OK;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/gzip"),
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    // A file name made of digits and fixed letters is always a valid header value.
    if let Ok(value) = HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response.into_response())
}

/// The reader's bytes in pieces; a read error ends the stream with that error.
fn chunks(
    reader: std::pin::Pin<Box<dyn AsyncRead + Send>>,
) -> impl futures_util::Stream<Item = std::io::Result<Vec<u8>>> {
    stream::unfold(Some(reader), |reader| async move {
        let mut reader = reader?;
        let mut buffer = vec![0_u8; CHUNK_BYTES];
        match reader.read(&mut buffer).await {
            Ok(0) => None,
            Ok(read) => {
                buffer.truncate(read);
                Some((Ok(buffer), Some(reader)))
            }
            // The error is the last item: the next poll finds no reader and ends the stream.
            Err(e) => Some((Err(e), None)),
        }
    })
}
