// SPDX-License-Identifier: AGPL-3.0-or-later
use ::http::HeaderValue;

use super::*;

const HTTP11: GuestHttp = GuestHttp::Http11 {
    close_requested: false,
};

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[test]
fn hop_by_hop_and_framing_headers_are_rebuilt() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "connection",
        HeaderValue::from_static("x-secret-hop, keep-alive"),
    );
    headers.insert("x-secret-hop", HeaderValue::from_static("1"));
    headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    headers.insert("content-type", HeaderValue::from_static("text/plain"));
    headers.insert("content-length", HeaderValue::from_static("99"));
    headers.append("set-cookie", HeaderValue::from_static("a=1"));
    headers.append("set-cookie", HeaderValue::from_static("b=2"));
    let head = text(&head_bytes(
        StatusCode::OK,
        &headers,
        Framing::Length(5),
        Written::KeepOpen,
    ));
    assert!(head.starts_with("HTTP/1.1 200 OK\r\n"));
    assert!(head.contains("content-type: text/plain\r\n"));
    assert!(head.contains("content-length: 5\r\n"));
    assert!(!head.contains("99") && !head.contains("x-secret-hop") && !head.contains("keep-alive"));
    assert_eq!(head.matches("set-cookie").count(), 2);
    assert!(!head.contains("connection:"));
    assert!(head.ends_with("\r\n\r\n"));
}

#[test]
fn head_and_not_modified_keep_their_length_and_204_has_none() {
    let mut headers = HeaderMap::new();
    headers.insert("content-length", HeaderValue::from_static("1234"));
    let head = text(&head_bytes(
        StatusCode::OK,
        &headers,
        Framing::None,
        Written::KeepOpen,
    ));
    assert!(head.contains("content-length: 1234\r\n"));
    let head = text(&head_bytes(
        StatusCode::NO_CONTENT,
        &headers,
        Framing::None,
        Written::KeepOpen,
    ));
    assert!(!head.contains("content-length"));
    let head = text(&head_bytes(
        StatusCode::OK,
        &HeaderMap::new(),
        Framing::Chunked,
        Written::Close,
    ));
    assert!(
        head.contains("transfer-encoding: chunked\r\n") && head.contains("connection: close\r\n")
    );
}

use bytes::Bytes;

async fn written(
    method: &str,
    status: StatusCode,
    guest: GuestHttp,
    body: &'static [u8],
) -> (String, Written) {
    let response = Response::builder()
        .status(status)
        .body(AnswerBody::Buffered(Some(Bytes::from_static(body))))
        .unwrap();
    let mut out = Vec::new();
    let how = write(&mut out, response, method, guest, Duration::from_secs(5))
        .await
        .unwrap();
    (text(&out), how)
}

#[tokio::test]
async fn an_answer_to_head_204_and_304_is_written_without_a_body_or_a_length_of_its_own() {
    for (method, status) in [
        ("HEAD", StatusCode::OK),
        ("head", StatusCode::OK),
        ("GET", StatusCode::NO_CONTENT),
        ("GET", StatusCode::NOT_MODIFIED),
    ] {
        let (out, _) = written(method, status, HTTP11, b"").await;
        assert!(
            !out.contains("content-length"),
            "{method} {status} must not get a length of its own: {out:?}"
        );
        assert!(out.ends_with("\r\n\r\n"), "{out:?}");
    }
    let (out, _) = written("GET", StatusCode::OK, HTTP11, b"abc").await;
    assert!(out.contains("content-length: 3\r\n") && out.ends_with("\r\n\r\nabc"));
}

#[tokio::test]
async fn an_http_1_0_guest_or_a_close_request_gets_the_connection_closed() {
    let (out, how) = written("GET", StatusCode::OK, GuestHttp::Http10, b"abc").await;
    assert!(out.contains("connection: close\r\n"), "{out:?}");
    assert!(matches!(how, Written::Close));
    let (out, how) = written("GET", StatusCode::OK, HTTP11, b"abc").await;
    assert!(!out.contains("connection:"), "{out:?}");
    assert!(matches!(how, Written::KeepOpen));
}

#[test]
fn closing_keeps_an_http_1_0_guest_and_marks_an_http_1_1_guest_as_asking_to_close() {
    assert_eq!(GuestHttp::Http10.closing(), GuestHttp::Http10);
    assert_eq!(
        GuestHttp::Http11 {
            close_requested: false
        }
        .closing(),
        GuestHttp::Http11 {
            close_requested: true
        }
    );
}
