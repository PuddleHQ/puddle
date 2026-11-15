// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests of the scan of answers for tokens Puddle does not hold: a token of a known kind in
//! an answer (header or body, split across chunks or frames, compressed) reaches the guest
//! unchanged, the connection record names its kind, one notice is raised for a host and kind, the
//! setting turns it off, a token Puddle holds is never found, and no value reaches a record, an
//! event or a log. Every token here is made up.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
use crate::terminate_support;

use std::io::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use http_body_util::BodyExt as _;
use puddle_proxy::{
    ForeignCredentialWarning, InjectDecision, SecretValue, StandIn, StandInOrigin, StandIns,
    TerminationSet,
};
use puddle_test_kit::{TokenKind, jwt, token};
use puddle_types::{CollectingSink, Event, UnknownTokenKind};
use terminate_support::h2_rig::{BoxError, H2Server, collect};
use terminate_support::{
    FakeServer, Flaw, Handler, Pki, Reply, RigBuilder, TestInjector, captured_logs,
};

fn pass_through() -> Arc<TestInjector> {
    TestInjector::new(|_| InjectDecision::PassThrough)
}

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

async fn upstream(pki: &Pki, handler: Handler) -> FakeServer {
    FakeServer::tls(pki.server_config("bound.test", Flaw::None), handler).await
}

fn notices(sink: &CollectingSink) -> Vec<(String, UnknownTokenKind)> {
    sink.events()
        .into_iter()
        .filter_map(|event| match event {
            Event::UnknownTokenInAnswer { host, kind, .. } => Some((host, kind)),
            _ => None,
        })
        .collect()
}

/// Nothing that reaches a record, an event or a log carries `value`.
fn assert_no_value(value: &str, record: &impl std::fmt::Debug, sink: &CollectingSink) {
    assert!(!format!("{record:?}").contains(value), "the record");
    assert!(!format!("{:?}", sink.events()).contains(value), "an event");
    assert!(!captured_logs().contains(value), "a log");
}

async fn get(rig: &terminate_support::Rig) -> terminate_support::Response {
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    client
        .send(b"GET /login HTTP/1.1\r\nHost: bound.test\r\n\r\n")
        .await;
    let response = client.response("GET").await;
    client.close().await;
    response
}

#[tokio::test]
async fn a_token_split_across_chunks_reaches_the_guest_and_the_record_and_one_notice_name_its_kind()
{
    let pki = Pki::new();
    let made = token(TokenKind::GithubPersonal, "chunks");
    let (a, b) = made.split_at(11);
    let body = [
        format!(r#"{{"access_token":"{a}"#).into_bytes(),
        format!(r#"{b}","token_type":"bearer"}}"#).into_bytes(),
    ];
    let reply = chunked("content-type: application/json\r\n", &[&body[0], &body[1]]);
    let server = upstream(&pki, Arc::new(move |_| Reply::raw(reply.clone()))).await;
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&sink)
        .build();
    for _ in 0..2 {
        let response = get(&rig).await;
        assert_eq!(response.status, 200);
        assert_eq!(
            response.text(),
            format!(r#"{{"access_token":"{made}","token_type":"bearer"}}"#),
            "the answer is not changed"
        );
    }
    let events = rig.events(2).await;
    for event in &events {
        assert_eq!(event.answer_credentials, ["github_token"]);
        assert_eq!(event.foreign_credentials, Vec::<String>::new());
    }
    assert_eq!(
        notices(&sink),
        [("bound.test".to_owned(), UnknownTokenKind::GithubToken)],
        "one notice for the host and kind, not one for each answer"
    );
    assert_no_value(&made, &events, &sink);
    assert_no_value(&made[8..], &events, &sink);
}

#[tokio::test]
async fn a_token_in_a_header_and_one_in_a_gzip_body_are_found() {
    let pki = Pki::new();
    let in_header = token(TokenKind::Npm, "header");
    let in_body = token(TokenKind::Anthropic, "gzip");
    let packed = gzip(format!(r#"{{"key":"{in_body}"}}"#).as_bytes());
    let mut reply = format!(
        "HTTP/1.1 200 OK\r\nset-cookie: s={in_header}; Path=/\r\ncontent-type: application/json\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\n\r\n",
        packed.len()
    )
    .into_bytes();
    reply.extend_from_slice(&packed);
    let server = upstream(&pki, Arc::new(move |_| Reply::raw(reply.clone()))).await;
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&sink)
        .build();
    let response = get(&rig).await;
    assert_eq!(response.status, 200);
    assert_eq!(response.header("content-encoding"), Some("gzip"));
    let events = rig.events(1).await;
    let mut kinds = events[0].answer_credentials.clone();
    kinds.sort();
    assert_eq!(kinds, ["anthropic_key", "npm_token"]);
    assert_no_value(&in_header, &events, &sink);
    assert_no_value(&in_body, &events, &sink);
}

#[tokio::test]
async fn a_jwt_next_to_a_refresh_token_field_is_found_and_a_jwt_alone_is_not() {
    let pki = Pki::new();
    let signed = jwt("login", r#"{"sub":"someone"}"#);
    let with_refresh =
        format!(r#"{{"id_token":"{signed}","refresh_token":"opaque-refresh-value"}}"#);
    let alone = format!(r#"{{"id_token":"{signed}"}}"#);
    let bodies = [with_refresh, alone];
    let turn = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = upstream(
        &pki,
        Arc::new(move |_| {
            let at = turn.fetch_add(1, Ordering::SeqCst);
            Reply::ok(&bodies[at])
        }),
    )
    .await;
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&sink)
        .build();
    assert_eq!(get(&rig).await.status, 200);
    assert_eq!(get(&rig).await.status, 200);
    let events = rig.events(2).await;
    assert_eq!(events[0].answer_credentials, ["jwt_with_refresh_token"]);
    assert_eq!(events[1].answer_credentials, Vec::<String>::new());
    assert_no_value(&signed, &events, &sink);
    assert_no_value("opaque-refresh-value", &events, &sink);
}

#[tokio::test]
async fn with_the_warning_off_the_answer_still_arrives_and_nothing_is_flagged() {
    let pki = Pki::new();
    let made = token(TokenKind::Gitlab, "off");
    let body = made.clone();
    let server = upstream(&pki, Arc::new(move |_| Reply::ok(&body))).await;
    let sink = Arc::new(CollectingSink::default());
    let on = Arc::new(AtomicBool::new(false));
    let switch = Arc::clone(&on);
    let warning = Arc::new(ForeignCredentialWarning::new(move || {
        switch.load(Ordering::Relaxed)
    }));
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .foreign_warning(&warning)
        .sink(&sink)
        .build();
    assert_eq!(get(&rig).await.text(), made);
    assert_eq!(notices(&sink), []);
    // The setting is read when a token is found, so turning it on applies to the next answer.
    on.store(true, Ordering::Relaxed);
    assert_eq!(get(&rig).await.text(), made);
    let events = rig.events(2).await;
    assert_eq!(events[0].answer_credentials, Vec::<String>::new());
    assert_eq!(events[1].answer_credentials, ["gitlab_token"]);
    assert_eq!(notices(&sink).len(), 1);
}

#[tokio::test]
async fn a_token_puddle_holds_is_never_found_even_where_the_answer_echoes_its_real_value() {
    let pki = Pki::new();
    let stand_in = token(TokenKind::GithubFineGrained, "mine");
    let real = token(TokenKind::GithubFineGrained, "real");
    let stranger = token(TokenKind::GithubFineGrained, "stranger");
    let stand_ins = Arc::new(StandIns::new());
    stand_ins
        .insert(
            StandIn::new(
                StandInOrigin::Secret,
                "HELD",
                &stand_in,
                SecretValue::new(&real),
                TerminationSet::parse(["bound.test"]).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
    let (echo, other) = (real.clone(), stranger.clone());
    let turn = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = upstream(
        &pki,
        Arc::new(move |_| {
            if turn.fetch_add(1, Ordering::SeqCst) == 0 {
                Reply::ok(&format!("echo {echo}"))
            } else {
                Reply::ok(&format!("echo {echo} {other}"))
            }
        }),
    )
    .await;
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .stand_ins(&stand_ins)
        .sink(&sink)
        .build();
    let response = get(&rig).await;
    assert_eq!(
        response.text(),
        format!("echo {stand_in}"),
        "scrubbed as ever"
    );
    assert_eq!(notices(&sink), []);

    // A token of the same kind that is not held, in the same answer, is found.
    let response = get(&rig).await;
    assert_eq!(response.text(), format!("echo {stand_in} {stranger}"));
    let events = rig.events(2).await;
    assert_eq!(events[0].answer_credentials, Vec::<String>::new());
    assert_eq!(events[1].answer_credentials, ["github_fine_grained_token"]);
    assert_no_value(&stranger, &events, &sink);
}

#[tokio::test]
async fn an_http2_answer_split_across_data_frames_is_read_and_the_record_names_the_kind() {
    let pki = Pki::new();
    let made = token(TokenKind::AzureDevops, "frames");
    let sent = made.clone();
    let handler: terminate_support::h2_rig::Handler = Arc::new(move |_, _| {
        let sent = sent.clone();
        Box::pin(async move {
            let frames = format!("token={sent}&x=1")
                .into_bytes()
                .chunks(5)
                .map(|piece| {
                    Ok::<_, BoxError>(http_body::Frame::data(bytes::Bytes::copy_from_slice(piece)))
                })
                .collect::<Vec<_>>();
            let body = http_body_util::StreamBody::new(futures_util::stream::iter(frames));
            http::Response::builder()
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body.boxed_unsync())
                .unwrap()
        })
    });
    let server = H2Server::streaming(&pki, "bound.test", handler).await;
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&sink)
        .build();
    let mut guest = rig.guest();
    let client = guest.h2("bound.test:443", &[b"h2"]).await;
    let mut sender = client.sender.clone();
    let request = http::Request::builder()
        .uri("https://bound.test/login")
        .body(
            http_body_util::Empty::<bytes::Bytes>::new()
                .map_err(|never| match never {})
                .boxed_unsync(),
        )
        .unwrap();
    let got = collect(sender.send_request(request).await.unwrap()).await;
    assert_eq!(got.text(), format!("token={made}&x=1"));
    drop(sender);
    client.close().await;
    let events = rig.events(1).await;
    assert_eq!(events[0].answer_credentials, ["azure_devops_token"]);
    assert_eq!(
        notices(&sink),
        [("bound.test".to_owned(), UnknownTokenKind::AzureDevopsToken)]
    );
    assert_no_value(&made, &events, &sink);
}

#[tokio::test]
async fn an_answer_that_is_not_text_is_not_read() {
    let pki = Pki::new();
    let made = token(TokenKind::GithubPersonal, "binary");
    let body = made.clone();
    let server = upstream(
        &pki,
        Arc::new(move |_| {
            Reply::raw(format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/octet-stream\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            ))
        }),
    )
    .await;
    let sink = Arc::new(CollectingSink::default());
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(pass_through())
        .sink(&sink)
        .build();
    assert_eq!(get(&rig).await.text(), made);
    let events = rig.events(1).await;
    assert_eq!(events[0].answer_credentials, Vec::<String>::new());
    assert_eq!(notices(&sink), []);
}
