// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const _: () = assert!(MAX_UPLOAD_BYTES > 4 * 1024 * 1024 * 1024);
const _: () = assert!(MAX_UPLOAD_BYTES < 5 * 1024 * 1024 * 1024);

#[test]
fn a_refusal_is_a_422_with_the_reason_and_a_missing_staging_is_a_404() {
    let refused = ApiError::from(RestoreServiceError::Refused("not a gzip archive".into()));
    assert_eq!(
        refused.status(),
        axum::http::StatusCode::UNPROCESSABLE_ENTITY
    );
    let gone = ApiError::from(RestoreServiceError::NotStaged("gone".into()));
    assert_eq!(gone.status(), axum::http::StatusCode::NOT_FOUND);
    let down = ApiError::from(RestoreServiceError::Failed("disk full".into()));
    assert_eq!(down.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
}
