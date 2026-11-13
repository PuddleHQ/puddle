// SPDX-License-Identifier: AGPL-3.0-or-later
#[derive(Debug)]
struct NoOverride;

impl ExchangeRewriter for NoOverride {
    fn begin(&self, _: &Host, _: &str, _: &str) -> Option<Exchange> {
        None
    }
}

#[test]
fn a_rewriter_names_no_token_endpoint_unless_it_says_so() {
    let host = Host::parse_normalised("tokens.test").unwrap();
    assert!(!NoOverride.is_token_endpoint(&host));
    assert!(NoOverride.begin(&host, "POST", "/").is_none());
}

use super::*;

struct Plain;

impl AnswerRewriter for Plain {
    fn wants(&mut self, _: &AnswerHead<'_>) -> bool {
        false
    }

    fn rewrite<'a>(
        &'a mut self,
        _: &'a AnswerHead<'a>,
        _: &'a [u8],
    ) -> BoxFuture<'a, Option<Bytes>> {
        Box::pin(std::future::ready(None))
    }
}

#[test]
fn an_exchange_says_what_it_asks_for_and_holds_nothing_a_log_should_not_have() {
    let exchange = Exchange::new()
        .swapping("refresh_token")
        .answered_by(Box::new(Plain));
    assert_eq!(
        format!("{exchange:?}"),
        r#"Exchange { swap_fields: ["refresh_token"], answer: true }"#
    );
    let nothing = Exchange::default();
    assert_eq!(
        format!("{nothing:?}"),
        "Exchange { swap_fields: [], answer: false }"
    );
    assert!(!nothing.wants_request_body());
}

#[tokio::test]
async fn an_answer_too_large_is_nothing_to_a_rewriter_that_does_not_care() {
    let mut plain = Plain;
    plain.too_large();
    let head = AnswerHead {
        status: 200,
        content_type: None,
        content_encoding: None,
        content_length: None,
    };
    assert!(!plain.wants(&head));
    assert_eq!(plain.rewrite(&head, b"{}").await, None);
}

#[tokio::test]
async fn a_buffered_answer_is_at_its_end_only_once_its_bytes_are_taken() {
    use http_body_util::BodyExt;
    let mut body = AnswerBody::Buffered(Some(Bytes::from_static(b"{}")));
    assert!(!body.is_end_stream());
    assert_eq!(
        body.frame().await.unwrap().unwrap().into_data().unwrap(),
        "{}"
    );
    assert!(body.is_end_stream());
}

#[test]
fn the_head_of_an_answer_is_read_from_its_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static(" 42 "));
    assert_eq!(
        AnswerHead::of(StatusCode::OK, &headers),
        AnswerHead {
            status: 200,
            content_type: Some("application/json"),
            content_encoding: Some("gzip"),
            content_length: Some(42),
        }
    );
    headers.insert(header::CONTENT_LENGTH, HeaderValue::from_static("many"));
    assert_eq!(
        AnswerHead::of(StatusCode::BAD_GATEWAY, &headers).content_length,
        None
    );
    assert_eq!(
        AnswerHead::of(StatusCode::OK, &HeaderMap::new()).content_type,
        None
    );
}

/// Wants every answer and counts the ones it was told were too large.
struct Wants {
    too_large: u32,
}

impl AnswerRewriter for Wants {
    fn wants(&mut self, _: &AnswerHead<'_>) -> bool {
        true
    }

    fn rewrite<'a>(
        &'a mut self,
        _: &'a AnswerHead<'a>,
        _: &'a [u8],
    ) -> BoxFuture<'a, Option<Bytes>> {
        Box::pin(std::future::ready(None))
    }

    fn too_large(&mut self) {
        self.too_large += 1;
    }
}

/// A response with a body of `len` bytes, read from a real HTTP/1 server over a pipe.
async fn response_of(len: usize) -> Response<Incoming> {
    use hyper_util::rt::TokioIo;
    let (client_end, server_end) = tokio::io::duplex(1 << 20);
    let service = hyper::service::service_fn(move |_| async move {
        Ok::<_, std::convert::Infallible>(Response::new(http_body_util::Full::new(Bytes::from(
            vec![b'x'; len],
        ))))
    });
    tokio::spawn(
        hyper::server::conn::http1::Builder::new()
            .serve_connection(TokioIo::new(server_end), service),
    );
    let (mut sender, connection) = hyper::client::conn::http1::handshake::<
        _,
        http_body_util::Empty<Bytes>,
    >(TokioIo::new(client_end))
    .await
    .unwrap();
    tokio::spawn(connection);
    sender
        .send_request(
            ::http::Request::get("/")
                .body(http_body_util::Empty::new())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn an_answer_of_exactly_the_limit_is_read_whole_and_one_byte_more_goes_through() {
    let mut wants = Wants { too_large: 0 };
    let at = read_answer(
        &mut wants,
        response_of(MAX_EXCHANGE_BODY).await,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(matches!(at.body(), AnswerBody::Buffered(_)));
    assert_eq!(wants.too_large, 0);
    let over = read_answer(
        &mut wants,
        response_of(MAX_EXCHANGE_BODY + 1).await,
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert!(matches!(over.body(), AnswerBody::Rejoined { .. }));
    assert_eq!(wants.too_large, 1);
}
