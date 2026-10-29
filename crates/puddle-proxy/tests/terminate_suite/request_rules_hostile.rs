// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules on method and path against hostile spellings: each request here reaches a server that
//! reads it as one the rule names (a router that matches on undecoded segments, a hop that decodes
//! the path and reads a `?` in it as the start of the query, a parser that takes `//name/` as an
//! authority, a framework that reads `.method` as `_method`, a WebSocket over HTTP/2), so each
//! must be refused before anything reaches the server.
#![expect(
    clippy::unwrap_used,
    reason = "a helper outside #[test] fails the test by panicking"
)]
use crate::terminate_support;

use std::sync::Arc;

use puddle_proxy::{RequestRule, RequestRules};
use puddle_types::{ConnectionDecision, Methods, PathPattern, RuleId};
use terminate_support::h2_rig::{H2Server, Script, reply};
use terminate_support::{FakeServer, Flaw, Pki, Reply, RigBuilder};

fn rule(id: i64, host: &str, methods: &[&str], path: &str) -> RequestRule {
    RequestRule::new(
        RuleId(id),
        host,
        Methods::parse(methods.iter().copied()).unwrap(),
        PathPattern::parse(path).unwrap(),
    )
    .unwrap()
}

/// One HTTP/1.1 request on a decrypted connection of its own to a host with `with`: the status
/// the guest got and how many requests reached the server.
async fn once(method: &str, target: &str, with: RequestRule) -> (u16, usize) {
    let pki = Pki::new();
    let server = FakeServer::tls(
        pki.server_config("api.test", Flaw::None),
        Arc::new(|_| Reply::ok("served")),
    )
    .await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(RequestRules::new(vec![with]))
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.tls("api.test:443", None).await.unwrap();
    client
        .send(
            format!("{method} {target} HTTP/1.1\r\nHost: api.test\r\nContent-Length: 0\r\n\r\n")
                .as_bytes(),
        )
        .await;
    let status = client.response(method).await.status;
    client.close().await;
    let events = rig.events(1).await;
    if status == 403 {
        assert_eq!(
            events.first().map(|e| e.decision),
            Some(ConnectionDecision::Deny),
            "{target}"
        );
    }
    (status, server.recorded().len())
}

#[tokio::test]
async fn an_encoded_slash_inside_one_segment_still_meets_a_one_segment_wildcard() {
    // A router that matches undecoded segments reads `a%2fb` as the one segment `a/b`.
    for target in ["/repos/a%2fb", "/repos/a%5Cb", "/repos/a%252fb"] {
        let (status, reached) = once(
            "DELETE",
            target,
            rule(1, "api.test", &["DELETE"], "/repos/*"),
        )
        .await;
        assert_eq!((status, reached), (403, 0), "{target}");
    }
}

#[tokio::test]
async fn a_question_mark_or_hash_that_only_decoding_reveals_still_ends_the_path() {
    // A hop that decodes the path before passing it on makes `%3f` the start of the query.
    for target in [
        "/admin%3fx=1",
        "/admin%23frag",
        "/admin%253f",
        "/admin%3F/../other",
    ] {
        let (status, reached) = once("GET", target, rule(2, "api.test", &["GET"], "/admin")).await;
        assert_eq!((status, reached), (403, 0), "{target}");
    }
}

#[tokio::test]
async fn a_path_that_starts_like_an_authority_is_also_judged_without_it() {
    // `//evil.test/admin` is the path `/admin` on `evil.test` to a parser that takes a leading
    // `//` as the start of an authority (a network-path reference, RFC 3986 §4.2).
    for target in [
        "//evil.test/admin",
        "/\\evil.test/admin",
        "/%2fevil.test/admin",
    ] {
        let (status, reached) = once("GET", target, rule(3, "api.test", &["GET"], "/admin")).await;
        assert_eq!((status, reached), (403, 0), "{target}");
    }
}

#[tokio::test]
async fn a_method_key_in_the_query_is_read_the_way_php_and_its_frameworks_read_it() {
    // PHP drops leading spaces of a query key and turns `.` and other spaces in it into `_`, and
    // reads `+` as a space.
    for target in [
        "/x?.method=DELETE",
        "/x?%20%20_method=DELETE",
        "/x?+.method=DELETE",
        "/x?_method=DELETE+",
        "/x?a=1;_method=DELETE",
    ] {
        let (status, reached) = once("POST", target, rule(4, "api.test", &["DELETE"], "/**")).await;
        assert_eq!((status, reached), (403, 0), "{target}");
    }
}

fn ok(text: &'static str) -> Script {
    Arc::new(move |_| reply(200, text))
}

#[tokio::test]
async fn a_websocket_over_http2_meets_a_rule_on_its_get() {
    // An extended CONNECT is how HTTP/2 spells the `GET` that opens a WebSocket on HTTP/1.1, and
    // an HTTP/1.1 server is sent exactly that `GET`.
    let pki = Pki::new();
    let server = H2Server::recording(&pki, "api.test", ok("served over h2")).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(RequestRules::new(vec![rule(
            5,
            "api.test",
            &["GET"],
            "/chat",
        )]))
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.h2("api.test:443", &[b"h2"]).await;
    let request = http::Request::builder()
        .method("CONNECT")
        .uri("https://api.test/chat")
        .header("sec-websocket-version", "13")
        .extension(hyper::ext::Protocol::from_static("websocket"))
        .body(terminate_support::h2_rig::open_body())
        .unwrap();
    let response = client.sender.send_request(request).await.unwrap();
    assert_eq!(response.status(), 403);
    assert_eq!(
        response.headers().get("x-puddle-request-rule").unwrap(),
        "5"
    );
    client.close().await;
    assert!(server.recorded().is_empty());
}
