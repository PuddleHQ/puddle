// SPDX-License-Identifier: GPL-3.0-or-later
//! axum extractors whose rejections are [`ApiErrorBody`](crate::ApiErrorBody) JSON like every
//! other error, instead of axum's plain-text ones.

use axum::RequestExt as _;
use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use http_body_util::BodyExt as _;
use serde::de::DeserializeOwned;
use serde_json::Value;
use zeroize::Zeroizing;

use crate::error::{ApiError, ErrorCode};

/// A JSON object body (`Content-Type: application/json` required). Anything but an object is
/// refused, so serde can't read a struct from an array.
#[derive(Debug)]
pub(crate) struct Json<T>(pub(crate) T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for Json<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, ApiError> {
        let axum::Json(value) = axum::Json::<Value>::from_request(request, state).await?;
        if !value.is_object() {
            return Err(ApiError::invalid("the body must be a JSON object"));
        }
        T::deserialize(value)
            .map(Json)
            .map_err(|err| ApiError::invalid(format!("invalid body: {err}")))
    }
}

/// What [`SecretJson`] starts its buffer with when the request says no length.
const BODY_START: usize = 1024;

/// A JSON object body that carries a secret (a pasted token, a secret's value): the same refusals
/// as [`Json`], but the bytes are collected into one buffer that is overwritten when it drops, and
/// the value is read straight from it, with no `Value` tree and no second copy in a `Bytes`.
///
/// What it cannot reach: the HTTP layer's own read buffer, which `hyper` reuses for the next
/// bytes, and (for a string with an escape in it) `serde_json`'s scratch space.
#[derive(Debug)]
pub(crate) struct SecretJson<T>(pub(crate) T);

impl<T: DeserializeOwned, S: Send + Sync> FromRequest<S> for SecretJson<T> {
    type Rejection = ApiError;

    async fn from_request(request: Request, _state: &S) -> Result<Self, ApiError> {
        if !is_json(request.headers()) {
            return Err(ApiError::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                ErrorCode::UnsupportedMediaType,
                "the body must be application/json",
            ));
        }
        let hint = request
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok());
        let body = collect(request.into_limited_body(), hint).await?;
        if body.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
            return Err(ApiError::invalid("the body must be a JSON object"));
        }
        // serde's own words only: `SecretText` refuses a wrong kind without quoting it.
        serde_json::from_slice(&body)
            .map(SecretJson)
            .map_err(|err| match err.classify() {
                serde_json::error::Category::Data => {
                    ApiError::invalid(format!("invalid body: {err}"))
                }
                _ => ApiError::bad_request(format!(
                    "Failed to parse the request body as JSON: {err}"
                )),
            })
    }
}

/// `application/json`, or any `application/*+json`, the way axum's own extractor reads it.
fn is_json(headers: &HeaderMap) -> bool {
    let Some(kind) = headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase())
    else {
        return false;
    };
    kind.strip_prefix("application/")
        .is_some_and(|sub| sub == "json" || sub.ends_with("+json"))
}

/// Whether `err`, or something it wraps, is the body limit being hit.
fn is_too_large(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut at = Some(err);
    while let Some(e) = at {
        if e.is::<http_body_util::LengthLimitError>() {
            return true;
        }
        at = e.source();
    }
    false
}

/// All of a body, in a buffer that wipes every allocation it leaves behind: the one it grows out
/// of is wiped too, not just freed.
async fn collect(
    mut body: axum::body::Body,
    hint: Option<usize>,
) -> Result<Zeroizing<Vec<u8>>, ApiError> {
    let mut buf = Zeroizing::new(Vec::with_capacity(hint.unwrap_or(BODY_START).min(1 << 20)));
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|err| {
            if is_too_large(&err) {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    ErrorCode::PayloadTooLarge,
                    "the body is too large",
                )
            } else {
                ApiError::bad_request("the body could not be read")
            }
        })?;
        if let Ok(data) = frame.into_data() {
            push(&mut buf, &data);
        }
    }
    Ok(buf)
}

/// Appends `data`. A buffer that is full is replaced by a bigger one and the old one wiped as it
/// drops, which `Vec`'s own growth would not do.
fn push(buf: &mut Zeroizing<Vec<u8>>, data: &[u8]) {
    let need = buf.len() + data.len();
    if need > buf.capacity() {
        let mut bigger = Zeroizing::new(Vec::with_capacity(need.max(buf.capacity() * 2)));
        bigger.extend_from_slice(buf);
        *buf = bigger;
    }
    buf.extend_from_slice(data);
}

/// Path parameters.
#[derive(Debug, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub(crate) struct Path<T>(pub(crate) T);

/// Query parameters.
#[derive(Debug, FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub(crate) struct Query<T>(pub(crate) T);

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        let message = rejection.body_text();
        match rejection {
            JsonRejection::MissingJsonContentType(_) => Self::new(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                ErrorCode::UnsupportedMediaType,
                "the body must be application/json",
            ),
            JsonRejection::BytesRejection(ref bytes)
                if bytes.status() == StatusCode::PAYLOAD_TOO_LARGE =>
            {
                Self::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    ErrorCode::PayloadTooLarge,
                    "the body is too large",
                )
            }
            _ => Self::bad_request(message),
        }
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        Self::bad_request(rejection.body_text())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buffer_keeps_every_byte_while_it_grows_past_its_first_size() {
        let mut buf = Zeroizing::new(Vec::with_capacity(4));
        let mut want = Vec::new();
        for chunk in [&b"ab"[..], b"cdef", b"", b"ghijklmnopqrstu", b"v"] {
            push(&mut buf, chunk);
            want.extend_from_slice(chunk);
            assert_eq!(*buf, want);
        }
        assert!(buf.capacity() >= want.len());
    }

    #[test]
    fn only_json_content_types_are_taken() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(CONTENT_TYPE, value.parse().unwrap());
            is_json(&headers)
        };
        assert!(!is_json(&HeaderMap::new()));
        assert!(with("application/json"));
        assert!(with("Application/JSON; charset=utf-8"));
        assert!(with("application/vnd.api+json"));
        assert!(!with("text/json"));
        assert!(!with("application/x-www-form-urlencoded"));
    }

    #[tokio::test]
    async fn a_body_that_breaks_off_is_a_bad_request_and_one_over_the_limit_is_too_large() {
        let broken = axum::body::Body::from_stream(futures_util::stream::iter([
            Ok(axum::body::Bytes::from_static(b"{\"a\"")),
            Err(std::io::Error::other("connection reset")),
        ]));
        let err = collect(broken, None).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);

        let big = axum::body::Body::new(http_body_util::Limited::new(
            axum::body::Body::from(vec![b'x'; 100]),
            10,
        ));
        let err = collect(big, Some(100)).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let fine = axum::body::Body::from("{\"a\":1}");
        assert_eq!(&**collect(fine, None).await.unwrap(), b"{\"a\":1}");
    }
}
