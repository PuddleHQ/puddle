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
use puddle_types::ReasonCode;

struct Plain;

impl AnswerRewriter for Plain {
    fn wants(&mut self, _: &AnswerHead<'_>) -> Want {
        Want::Pass
    }

    fn rewrite<'a>(&'a mut self, _: &'a AnswerHead<'a>, _: &'a [u8]) -> BoxFuture<'a, Rewrite> {
        Box::pin(std::future::ready(Rewrite::Keep))
    }
}

#[test]
fn an_exchange_says_what_it_asks_for_and_holds_nothing_a_log_should_not_have() {
    let exchange = Exchange::new()
        .swapping("refresh_token")
        .answered_by(Box::new(Plain));
    assert_eq!(
        format!("{exchange:?}"),
        r#"Exchange { swap_fields: ["refresh_token"], answer: true, before: false }"#
    );
    let nothing = Exchange::default();
    assert_eq!(
        format!("{nothing:?}"),
        "Exchange { swap_fields: [], answer: false, before: false }"
    );
    let later = Exchange::new().after(Box::pin(std::future::ready(())));
    assert_eq!(
        format!("{later:?}"),
        "Exchange { swap_fields: [], answer: false, before: true }"
    );
    assert!(!nothing.wants_request_body());
}

#[tokio::test]
async fn an_answer_too_large_is_nothing_to_a_rewriter_that_does_not_care() {
    let mut plain = Plain;
    assert_eq!(plain.too_large(), Rewrite::Keep);
    let head = AnswerHead {
        status: 200,
        content_type: None,
        content_encoding: None,
        content_length: None,
    };
    assert_eq!(plain.wants(&head), Want::Pass);
    assert_eq!(plain.rewrite(&head, b"{}").await, Rewrite::Keep);
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
    fn wants(&mut self, _: &AnswerHead<'_>) -> Want {
        Want::Read
    }

    fn rewrite<'a>(&'a mut self, _: &'a AnswerHead<'a>, _: &'a [u8]) -> BoxFuture<'a, Rewrite> {
        Box::pin(std::future::ready(Rewrite::Keep))
    }

    fn too_large(&mut self) -> Rewrite {
        self.too_large += 1;
        Rewrite::Keep
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

fn no_way() -> InjectRefusal {
    InjectRefusal::new(502, ReasonCode::CredentialUnavailable, "not for the guest")
}

/// Says the same thing at each of the three points a rewriter can speak.
struct Scripted {
    want: Want,
    rewrite: Rewrite,
    too_large: Rewrite,
}

impl AnswerRewriter for Scripted {
    fn wants(&mut self, _: &AnswerHead<'_>) -> Want {
        self.want.clone()
    }

    fn rewrite<'a>(&'a mut self, _: &'a AnswerHead<'a>, _: &'a [u8]) -> BoxFuture<'a, Rewrite> {
        Box::pin(std::future::ready(self.rewrite.clone()))
    }

    fn too_large(&mut self) -> Rewrite {
        self.too_large.clone()
    }
}

async fn read_with(rewriter: Scripted, len: usize) -> Result<Response<AnswerBody>, AnswerError> {
    let mut rewriter = rewriter;
    read_answer(
        &mut rewriter,
        response_of(len).await,
        Duration::from_secs(5),
    )
    .await
}

fn refused(result: Result<Response<AnswerBody>, AnswerError>) -> bool {
    matches!(result, Err(AnswerError::Refused(r)) if r == no_way())
}

#[tokio::test]
async fn each_way_a_rewriter_refuses_an_answer_reaches_the_guest_as_a_refusal() {
    let keep = || Scripted {
        want: Want::Read,
        rewrite: Rewrite::Keep,
        too_large: Rewrite::Keep,
    };
    assert!(refused(
        read_with(
            Scripted {
                want: Want::Refuse(no_way()),
                ..keep()
            },
            2
        )
        .await
    ));
    assert!(refused(
        read_with(
            Scripted {
                rewrite: Rewrite::Refuse(no_way()),
                ..keep()
            },
            2
        )
        .await
    ));
    assert!(refused(
        read_with(
            Scripted {
                too_large: Rewrite::Refuse(no_way()),
                ..keep()
            },
            MAX_EXCHANGE_BODY + 1
        )
        .await
    ));
}

#[tokio::test]
async fn keep_pass_and_replace_are_the_only_ways_an_answer_gets_through() {
    let pass = read_with(
        Scripted {
            want: Want::Pass,
            rewrite: Rewrite::Refuse(no_way()),
            too_large: Rewrite::Refuse(no_way()),
        },
        2,
    )
    .await
    .unwrap();
    assert!(matches!(pass.body(), AnswerBody::Streaming(_)));
    let replaced = read_with(
        Scripted {
            want: Want::Read,
            rewrite: Rewrite::Replace(Bytes::from_static(b"new")),
            too_large: Rewrite::Keep,
        },
        2,
    )
    .await
    .unwrap();
    assert!(matches!(replaced.body(), AnswerBody::Buffered(Some(b)) if b == "new"));
}

#[tokio::test]
async fn the_work_an_exchange_asks_for_runs_once_when_it_is_prepared() {
    let ran = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = std::sync::Arc::clone(&ran);
    let mut exchange = Exchange::new().after(Box::pin(async move {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }));
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 0);
    exchange.prepare().await;
    exchange.prepare().await;
    assert_eq!(ran.load(std::sync::atomic::Ordering::SeqCst), 1);
}
