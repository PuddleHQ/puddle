// SPDX-License-Identifier: AGPL-3.0-or-later
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
