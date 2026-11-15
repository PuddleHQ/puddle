// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests of answers from a host a secret is for: a real value the server hands back, in a
//! header or anywhere in the body, split across chunks or frames, compressed or encoded, reaches
//! the guest as the stand-in it already holds, and the record says so.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::io::Write as _;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use http_body_util::BodyExt as _;
use puddle_proxy::{
    InjectDecision, SecretValue, StandIn, StandInOrigin, StandIns, TerminationSet, secret_stand_in,
};
use puddle_types::ConnectionReason;
use terminate_support::h2_rig::{BoxError, H2Server, collect};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Reply, RigBuilder, TestInjector, captured_logs,
};

const REAL: &str = "sk-live-5c1d9e0a47b2/echo";

fn registry() -> (Arc<StandIns>, String) {
    let stand_ins = Arc::new(StandIns::new());
    let stand_in = secret_stand_in("API_KEY").unwrap();
    stand_ins
        .insert(
            StandIn::new(
                StandInOrigin::Secret,
                "API_KEY",
                &stand_in,
                SecretValue::new(REAL),
                TerminationSet::parse(["bound.test"]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    (stand_ins, stand_in)
}

fn pass_through() -> Arc<TestInjector> {
    TestInjector::new(|_| InjectDecision::PassThrough)
}

/// An HTTP/1.1 answer whose body comes in `chunks`, after `head` (header lines).
fn chunked(head: &str, chunks: &[&[u8]]) -> Vec<u8> {
    let mut out =
        format!("HTTP/1.1 200 OK\r\n{head}transfer-encoding: chunked\r\n\r\n").into_bytes();
    for chunk in chunks {
        out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
        out.extend_from_slice(chunk);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

async fn h1_upstream(pki: &Pki, handler: Handler) -> FakeServer {
    FakeServer::tls(pki.server_config("bound.test", Flaw::None), handler).await
}

#[tokio::test]
async fn a_real_value_echoed_in_headers_and_a_chunked_body_comes_back_as_the_stand_in() {
    let pki = Pki::new();
    let (cut_a, cut_b) = REAL.split_at(7);
    let escaped = REAL.replace('/', "%2F");
    let json = REAL.replace('/', "\\/");
    let b64 = STANDARD.encode(REAL);
    let body = [
        format!("{{\"auth\":\"Bearer {cut_a}").into_bytes(),
        format!("{cut_b}\",\"php\":\"{json}\",\"b64\":\"{b64}\"}}").into_bytes(),
    ];
    let reply = chunked(
        &format!(
            "set-cookie: k={REAL}; Path=/\r\nlocation: https://bound.test/cb?k={escaped}\r\ncontent-type: application/json\r\n"
        ),
        &[&body[0], &body[1]],
    );
    let server = h1_upstream(&pki, Arc::new(move |_| Reply::raw(reply.clone()))).await;
    let (stand_ins, stand_in) = registry();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&stand_ins)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(format!("GET /debug HTTP/1.1\r\nHost: bound.test\r\nAuthorization: Bearer {stand_in}\r\nAccept-Encoding: gzip, compress, dcb, br\r\n\r\n").as_bytes())
        .await;
    let response = client.response("GET").await;
    assert_eq!(response.status, 200);
    let seen = format!("{response:?}{}", response.text());
    assert!(!seen.contains(REAL) && !seen.contains(&escaped) && !seen.contains(&b64));
    assert_eq!(
        response.header("set-cookie"),
        Some(format!("k={stand_in}; Path=/").as_str())
    );
    assert_eq!(
        response.header("location"),
        Some(format!("https://bound.test/cb?k={stand_in}").as_str())
    );
    assert_eq!(
        response.text(),
        format!(
            "{{\"auth\":\"Bearer {stand_in}\",\"php\":\"{stand_in}\",\"b64\":\"{}\"}}",
            STANDARD.encode(&stand_in)
        )
    );
    let request = &server.recorded()[0];
    assert_eq!(
        request.header("accept-encoding"),
        Some("gzip, br"),
        "only what Puddle decodes is asked for"
    );
    client.close().await;
    let event = &rig.events(1).await[0];
    assert_eq!(event.scrubbed_stand_ins, ["stand-in:secret:API_KEY"]);
    assert!(!format!("{event:?}").contains(REAL));
    assert!(
        !captured_logs().contains(REAL),
        "the real value reached a log"
    );
}

#[tokio::test]
async fn hostile_a_server_that_trickles_a_gzip_answer_byte_by_byte_still_gives_the_stand_in() {
    let pki = Pki::new();
    let compressed = gzip(format!("<pre>token={REAL}</pre>").as_bytes());
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\n\r\n",
        compressed.len()
    )
    .into_bytes();
    reply.extend_from_slice(&compressed);
    let server = h1_upstream(
        &pki,
        Arc::new(move |_| Reply::raw(reply.clone()).trickling(Duration::from_millis(1))),
    )
    .await;
    let (stand_ins, stand_in) = registry();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&stand_ins)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(b"GET / HTTP/1.1\r\nHost: bound.test\r\nAccept-Encoding: gzip\r\n\r\n")
        .await;
    let response = client.response("GET").await;
    assert_eq!(response.text(), format!("<pre>token={stand_in}</pre>"));
    assert_eq!(response.header("content-encoding"), None, "sent plain");
    client.close().await;
    assert_eq!(
        rig.events(1).await[0].scrubbed_stand_ins,
        ["stand-in:secret:API_KEY"]
    );
}

#[tokio::test]
async fn an_answer_in_an_encoding_puddle_cannot_read_is_refused_with_its_reason() {
    let pki = Pki::new();
    let server = h1_upstream(
        &pki,
        Arc::new(|_| {
            Reply::raw(
                "HTTP/1.1 200 OK\r\ncontent-encoding: compress\r\ncontent-length: 3\r\n\r\nabc",
            )
        }),
    )
    .await;
    let (stand_ins, _) = registry();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&stand_ins)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(b"GET / HTTP/1.1\r\nHost: bound.test\r\n\r\n")
        .await;
    let response = client.response("GET").await;
    assert_eq!(response.status, 502);
    assert_eq!(response.header("x-puddle-blocked"), Some("answer_encoding"));
    drop(client);
    let event = &rig.events(1).await[0];
    assert_eq!(event.reason, ConnectionReason::Refused("answer_encoding"));
}

#[tokio::test]
async fn hostile_a_real_value_inside_longer_base64_cuts_the_answer_off_before_it() {
    let pki = Pki::new();
    let blob = STANDARD.encode(format!("user:{REAL}"));
    let body = format!("{{\"echo\":\"Basic {blob}\"}}");
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    );
    let server = h1_upstream(&pki, Arc::new(move |_| Reply::raw(reply.clone()))).await;
    let (stand_ins, _) = registry();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&stand_ins)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(b"GET / HTTP/1.1\r\nHost: bound.test\r\n\r\n")
        .await;
    assert!(
        client.try_response("GET").await.is_none(),
        "the answer is cut off, never whole"
    );
    drop(client);
    let event = &rig.events(1).await[0];
    assert_eq!(event.reason, ConnectionReason::Refused("answer_withheld"));
}

#[tokio::test]
async fn an_http2_answer_split_across_data_frames_and_its_trailers_get_the_stand_in() {
    let pki = Pki::new();
    let handler: terminate_support::h2_rig::Handler = Arc::new(|_, _| {
        Box::pin(async move {
            let mut trailers = http::HeaderMap::new();
            trailers.insert("x-echo", REAL.parse().unwrap());
            let frames = REAL
                .as_bytes()
                .chunks(3)
                .map(|piece| {
                    Ok::<_, BoxError>(http_body::Frame::data(bytes::Bytes::copy_from_slice(piece)))
                })
                .chain([Ok(http_body::Frame::trailers(trailers))])
                .collect::<Vec<_>>();
            let body = http_body_util::StreamBody::new(futures_util::stream::iter(frames));
            http::Response::builder()
                .header("x-token", REAL)
                .body(body.boxed_unsync())
                .unwrap()
        })
    });
    let server = H2Server::streaming(&pki, "bound.test", handler).await;
    let (stand_ins, stand_in) = registry();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&stand_ins)
        .build();
    let mut guest = rig.guest();
    let client = guest.h2("bound.test:443", &[b"h2"]).await;
    let mut sender = client.sender.clone();
    let request = http::Request::builder()
        .uri("https://bound.test/echo")
        .header("te", "trailers")
        .body(
            http_body_util::Empty::<bytes::Bytes>::new()
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let got = collect(sender.send_request(request).await.unwrap()).await;
    assert_eq!(got.text(), stand_in);
    assert_eq!(got.header("x-token"), Some(stand_in.as_str()));
    let trailers = got.trailers.clone().unwrap();
    assert!(
        trailers.contains(&("x-echo".to_owned(), stand_in.clone())),
        "{trailers:?}"
    );
    drop(sender);
    client.close().await;
    let event = &rig.events(1).await[0];
    assert_eq!(event.scrubbed_stand_ins, ["stand-in:secret:API_KEY"]);
}

#[tokio::test]
async fn a_head_answer_gets_its_headers_scrubbed_and_keeps_its_length() {
    let pki = Pki::new();
    let server = h1_upstream(
        &pki,
        Arc::new(|_| {
            Reply::raw(format!(
                "HTTP/1.1 200 OK\r\nset-cookie: k={REAL}\r\ncontent-length: 1234\r\n\r\n"
            ))
        }),
    )
    .await;
    let (stand_ins, stand_in) = registry();
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&stand_ins)
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(b"HEAD / HTTP/1.1\r\nHost: bound.test\r\n\r\n")
        .await;
    let response = client.response("HEAD").await;
    assert_eq!(
        response.header("set-cookie"),
        Some(format!("k={stand_in}").as_str())
    );
    assert_eq!(response.header("content-length"), Some("1234"));
    assert_eq!(response.body, b"");
    client.close().await;
    assert_eq!(
        rig.events(1).await[0].scrubbed_stand_ins,
        ["stand-in:secret:API_KEY"]
    );
}
