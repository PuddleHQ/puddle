// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules on method and path: what a refused request looks like, that every spelling of a path
//! meets its rule on HTTP/1.1, HTTP/2 and plain HTTP, that a host with a rule is always decrypted
//! and that nothing of a refused request's query reaches the audit.
#![expect(
    clippy::unwrap_used,
    reason = "a helper outside #[test] fails the test by panicking"
)]
use crate::terminate_support;

use std::sync::Arc;
use std::time::Duration;

use puddle_proxy::{PassThrough, RequestRule, RequestRules};
use puddle_types::{ConnectionDecision, Methods, PathPattern, RuleId};
use rustls::pki_types::ServerName;
use terminate_support::h2_rig::{H2Server, Script, full, reply};
use terminate_support::{
    CANARY, Client, FakeServer, Flaw, Pki, Reply, RigBuilder, TestInjector, host,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

fn rule(id: i64, host: &str, methods: &[&str], path: &str) -> RequestRule {
    RequestRule::new(
        RuleId(id),
        host,
        Methods::parse(methods.iter().copied()).unwrap(),
        PathPattern::parse(path).unwrap(),
    )
    .unwrap()
}

fn rules(list: Vec<RequestRule>) -> RequestRules {
    RequestRules::new(list)
}

async fn api(pki: &Pki) -> FakeServer {
    FakeServer::tls(
        pki.server_config("api.test", Flaw::None),
        Arc::new(|_| Reply::ok("served")),
    )
    .await
}

/// Sends one request on a decrypted connection and reads the answer.
async fn send(
    client: &mut Client,
    method: &str,
    target: &str,
    extra: &str,
) -> terminate_support::Response {
    client
        .send(
            format!(
                "{method} {target} HTTP/1.1\r\nHost: api.test\r\nContent-Length: 0\r\n{extra}\r\n"
            )
            .as_bytes(),
        )
        .await;
    client.response(method).await
}

/// A request on a connection of its own, as the answer and what the server saw.
async fn once(
    method: &str,
    target: &str,
    extra: &str,
    with: Vec<RequestRule>,
) -> (u16, usize, Vec<puddle_types::ConnectionEvent>) {
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(rules(with))
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.tls("api.test:443", None).await.unwrap();
    let answer = send(&mut client, method, target, extra).await;
    client.close().await;
    let events = rig.events(1).await;
    (answer.status, server.recorded().len(), events)
}

#[tokio::test]
async fn a_refused_request_is_a_403_that_names_the_rule_and_never_reaches_the_server() {
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(rules(vec![rule(7, "api.test", &["POST"], "/gists")]))
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.tls("api.test:443", None).await.unwrap();
    let answer = send(&mut client, "POST", &format!("/gists?token={CANARY}"), "").await;
    assert_eq!(answer.status, 403);
    assert_eq!(answer.header("x-puddle-decision"), Some("deny"));
    assert_eq!(answer.header("x-puddle-request-rule"), Some("7"));
    assert!(
        answer.text().contains("request rule 7"),
        "{}",
        answer.text()
    );
    assert!(!answer.text().contains("gists"), "the path is not echoed");
    client.close().await;
    assert!(server.recorded().is_empty(), "nothing went upstream");

    let events = rig.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Deny);
    assert_eq!(events[0].reason.to_string(), "request_rule");
    assert_eq!(events[0].request_rule, Some(RuleId(7)));
    let line = events[0].http.as_ref().expect("the refused request");
    assert_eq!((line.method(), line.path()), ("POST", "/gists"));
    assert!(
        !format!("{events:?}").contains(CANARY),
        "the query string is not in the record"
    );
}

#[tokio::test]
async fn only_the_requests_a_rule_names_are_refused() {
    for (method, target) in [
        ("GET", "/gists"),
        ("POST", "/gists/1"),
        ("POST", "/gist"),
        ("POST", "/"),
        ("DELETE", "/gists"),
    ] {
        let (status, upstream, events) = once(
            method,
            target,
            "",
            vec![rule(7, "api.test", &["POST"], "/gists")],
        )
        .await;
        assert_eq!((status, upstream), (200, 1), "{method} {target}");
        assert_eq!(events[0].decision, ConnectionDecision::Allow);
        assert_eq!(events[0].request_rule, None);
    }
    // A rule with no method refuses every method.
    for method in ["GET", "PATCH", "OPTIONS"] {
        let (status, upstream, _) =
            once(method, "/x/y", "", vec![rule(1, "api.test", &[], "/**")]).await;
        assert_eq!((status, upstream), (403, 0), "{method}");
    }
}

#[tokio::test]
async fn the_second_request_of_a_connection_is_judged_on_its_own() {
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(rules(vec![rule(7, "api.test", &["DELETE"], "/repos/*")]))
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.tls("api.test:443", None).await.unwrap();
    assert_eq!(send(&mut client, "GET", "/repos/x", "").await.status, 200);
    let refused = send(&mut client, "DELETE", "/repos/x", "").await;
    assert_eq!(refused.status, 403);
    client.close().await;
    assert_eq!(server.recorded().len(), 1, "only the GET went upstream");
    let events = rig.events(1).await;
    let line = events[0].http.as_ref().unwrap();
    assert_eq!(
        (line.method(), line.path(), events[0].decision),
        ("DELETE", "/repos/x", ConnectionDecision::Deny),
        "the record names the refused request, not the first one"
    );
}

/// Every way to write `/admin` that a server may read as that path.
const SPELLINGS: &[&str] = &[
    "/admin",
    "/ADMIN",
    "/Admin/",
    "//admin",
    "///admin//",
    "/./admin",
    "/x/../admin",
    "/x/y/../../admin",
    "/%61dmin",
    "/%41dmin",
    "/a%64min",
    "/%2fadmin",
    "/x/%2e%2e/admin",
    "/x/%2E%2E/admin",
    "/x/.%2e/admin",
    "/x%2f..%2fadmin",
    "/x%5c..%5cadmin",
    "/%252e%252e/admin",
    "/x/%252e%252e/admin",
    "/admin;a=b",
    "/admin;a=b/",
    "/;a=b/admin",
    "/admin.",
    "/admin%20",
    "/admin%2e",
    "/admin%00",
    "/admin%00.png",
    "/%u0061dmin",
    "/..%2f..%2fadmin",
    "/../admin",
    "/admin?x=1",
    "/admin?x=1#f",
    "/admin/?",
    "/admin?/../other",
    "/admin?%2e%2e",
    "http://api.test/admin",
    "http://api.test:443/%61dmin",
    "http://user:pw@api.test/admin",
];

#[tokio::test]
async fn no_spelling_of_a_refused_path_gets_through_on_http_1_1() {
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(rules(vec![rule(9, "api.test", &["DELETE"], "/admin")]))
        .build();
    for spelling in SPELLINGS {
        let mut guest = rig.guest().await;
        let mut client = guest.tls("api.test:443", None).await.unwrap();
        let answer = send(&mut client, "DELETE", spelling, "").await;
        // 400 is the parser's own refusal of a form it will not read at all: it never goes
        // upstream either.
        assert!(
            matches!(answer.status, 403 | 400),
            "{spelling}: {}",
            answer.status
        );
        if answer.status == 403 {
            assert_eq!(
                answer.header("x-puddle-request-rule"),
                Some("9"),
                "{spelling}"
            );
        }
        client.close().await;
    }
    assert!(
        server.recorded().is_empty(),
        "reached the server: {:?}",
        server.recorded()
    );
    // The plain spelling and the ones a parser reads are refused by the rule itself.
    for spelling in [
        "/admin",
        "/ADMIN",
        "//admin",
        "/%61dmin",
        "/x/%2e%2e/admin",
        "/admin;a=b",
    ] {
        let mut guest = rig.guest().await;
        let mut client = guest.tls("api.test:443", None).await.unwrap();
        assert_eq!(
            send(&mut client, "DELETE", spelling, "").await.status,
            403,
            "{spelling}"
        );
        client.close().await;
    }
}

#[tokio::test]
async fn paths_that_are_not_the_refused_one_are_served() {
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(rules(vec![rule(9, "api.test", &["DELETE"], "/admin")]))
        .build();
    let ok = [
        "/administrator",
        "/admin/users",
        "/x/admin",
        "/admin2",
        "/a/%2e%2e/b",
        "/",
        "/%61dmin/x",
    ];
    for target in ok {
        let mut guest = rig.guest().await;
        let mut client = guest.tls("api.test:443", None).await.unwrap();
        assert_eq!(
            send(&mut client, "DELETE", target, "").await.status,
            200,
            "{target}"
        );
        client.close().await;
    }
    assert_eq!(server.recorded().len(), ok.len());
}

#[tokio::test]
async fn a_method_asked_for_by_header_or_query_is_the_method_judged() {
    let rule_for = || vec![rule(5, "api.test", &["DELETE"], "/repos/**")];
    for (extra, target) in [
        ("X-HTTP-Method-Override: DELETE\r\n", "/repos/a"),
        ("x-http-method-override: delete\r\n", "/repos/a"),
        ("X-HTTP-Method: DELETE\r\n", "/repos/a"),
        ("X-Method-Override: PUT, DELETE\r\n", "/repos/a"),
        ("", "/repos/a?_method=DELETE"),
        ("", "/repos/a?x=1&_method=delete"),
        ("", "/repos/a?_method=%44ELETE"),
        ("", "/repos/a?_%6Dethod=DELETE"),
    ] {
        let (status, upstream, events) = once("POST", target, extra, rule_for()).await;
        assert_eq!((status, upstream), (403, 0), "{extra:?} {target}");
        assert_eq!(events[0].request_rule, Some(RuleId(5)));
        let line = events[0].http.as_ref().unwrap();
        assert_eq!(
            (line.method(), line.path()),
            ("POST", target.split('?').next().unwrap())
        );
    }
    // An override to another method is no DELETE.
    let (status, upstream, _) = once(
        "POST",
        "/repos/a?_method=PUT",
        "X-HTTP-Method-Override: PATCH\r\n",
        rule_for(),
    )
    .await;
    assert_eq!((status, upstream), (200, 1));
}

#[tokio::test]
async fn a_host_with_a_rule_is_decrypted_whatever_the_switch_and_the_never_decrypt_list_say() {
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .injector(TestInjector::new(|_| {
            puddle_proxy::InjectDecision::PassThrough
        }))
        .decrypting_all(false, &["api.test"])
        .request_rules(rules(vec![rule(2, "api.test", &["DELETE"], "/**")]))
        .build();
    let mut guest = rig.guest().await;
    // The workspace's CA certifies the name (a client trusting only the real root would refuse).
    let mut client = guest.tls("api.test:443", None).await.unwrap();
    assert_eq!(send(&mut client, "GET", "/", "").await.status, 200);
    client.close().await;
    assert!(
        !rig.events(1).await[0]
            .reason
            .to_string()
            .starts_with("pass_through")
    );
}

#[tokio::test]
async fn a_host_with_a_rule_is_decrypted_and_judged_for_a_client_in_a_container_too() {
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .injector(TestInjector::new(|_| {
            puddle_proxy::InjectDecision::PassThrough
        }))
        .decrypting_all(true, &[])
        .request_rules(rules(vec![rule(4, "api.test", &["POST"], "/gists")]))
        .build();
    // A container's client is passed through on other hosts; this one gets the workspace's
    // certificate because its requests must be read.
    let mut container = rig.guest().await.in_container();
    let mut client = container.tls("api.test:443", None).await.unwrap();
    assert_eq!(send(&mut client, "POST", "/gists", "").await.status, 403);
    client.close().await;
    assert!(server.recorded().is_empty(), "nothing went upstream");
    let events = rig.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Deny);
    assert_eq!(events[0].reason.to_string(), "request_rule");
    assert_eq!(events[0].request_rule, Some(RuleId(4)));
    // The mark does not open a tunnel to another port of the host either.
    let (status, _) = container.connect_to("api.test:8443").await;
    assert_eq!(status, 403);
    assert_eq!(
        rig.events(2).await[1].reason.to_string(),
        "request_rule_port"
    );
}

#[tokio::test]
async fn a_tunnel_to_another_port_of_a_covered_host_is_refused_because_its_requests_cannot_be_read()
{
    let pki = Pki::new();
    let server = api(&pki).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test", "other.test"])
        .request_rules(rules(vec![rule(2, "api.test", &["DELETE"], "/**")]))
        .build();
    let mut guest = rig.guest().await;
    let (status, _) = guest.connect_to("api.test:8443").await;
    assert_eq!(status, 403);
    let events = rig.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Blocked);
    assert_eq!(events[0].reason.to_string(), "request_rule_port");
    // A host with no rule keeps its tunnels.
    let (status, _) = guest.connect_to("other.test:8443").await;
    assert_ne!(status, 403);
    assert!(server.recorded().is_empty());
}

#[tokio::test]
async fn a_plain_http_request_is_judged_like_one_on_a_decrypted_connection() {
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&seen);
    let server = FakeServer::plain(Arc::new(move |_| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Reply::ok("plain")
    }))
    .await;
    let rig = RigBuilder::new(&Pki::new())
        .name("plain.test", server.addr)
        .allow(vec!["plain.test"])
        .request_rules(rules(vec![rule(4, "plain.test", &["DELETE"], "/admin")]))
        .build();
    for target in [
        "http://plain.test/admin",
        "http://plain.test/%41dmin/./?q=1",
        "http://plain.test//x/../ADMIN",
    ] {
        let guest = rig.guest().await;
        let mut stream = guest.control.clone().open_stream().await.unwrap();
        stream
            .write_all(
                format!(
                    "DELETE {target} HTTP/1.1\r\nHost: plain.test\r\nContent-Length: 0\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(line.starts_with("HTTP/1.1 403"), "{target}: {line}");
    }
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 0);
    let events = rig.events(3).await;
    for event in &events {
        assert_eq!(event.reason.to_string(), "request_rule");
        assert_eq!(event.request_rule, Some(RuleId(4)));
        assert_eq!(event.http.as_ref().unwrap().method(), "DELETE");
        assert!(!format!("{event:?}").contains("q=1"));
    }
    // Another method on the same path is served.
    let guest = rig.guest().await;
    let mut stream = guest.control.clone().open_stream().await.unwrap();
    stream
        .write_all(b"GET http://plain.test/admin HTTP/1.1\r\nHost: plain.test\r\n\r\n")
        .await
        .unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(line.starts_with("HTTP/1.1 200"), "{line}");
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    let _ = host("plain.test");
}

fn ok(text: &'static str) -> Script {
    Arc::new(move |_| reply(200, text))
}

#[tokio::test]
async fn on_http_2_one_stream_is_refused_and_the_connection_goes_on() {
    let pki = Pki::new();
    let server = H2Server::recording(&pki, "api.test", ok("served over h2")).await;
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .request_rules(rules(vec![rule(8, "api.test", &["DELETE"], "/admin")]))
        .build();
    let mut guest = rig.guest().await;
    let mut client = guest.h2("api.test:443", &[b"h2"]).await;
    let ok = client.get("api.test", "/admin").await;
    assert_eq!(ok.status, 200);
    for spelling in [
        "/admin",
        "/ADMIN/",
        "//admin",
        "/%61dmin",
        "/x/%2e%2e/admin",
        "/%252e%252e/admin",
        "/admin;a=b",
        "/admin%00.png",
        "/admin?q=secret",
    ] {
        let got = client
            .request(
                "DELETE",
                "api.test",
                spelling,
                &[],
                full(bytes::Bytes::new()),
            )
            .await;
        assert_eq!(got.status, 403, "{spelling}");
        assert_eq!(got.header("x-puddle-request-rule"), Some("8"), "{spelling}");
    }
    let got = client
        .request(
            "POST",
            "api.test",
            "/admin",
            &[("x-http-method-override", "DELETE")],
            full(bytes::Bytes::new()),
        )
        .await;
    assert_eq!(got.status, 403);
    let after = client.get("api.test", "/admin/users").await;
    assert_eq!(after.status, 200, "the connection still works");
    client.close().await;
    assert_eq!(
        server.recorded().len(),
        2,
        "only the two GETs reached the server"
    );
    let events = rig.events(1).await;
    assert_eq!(events[0].reason.to_string(), "request_rule");
    assert_eq!(events[0].request_rule, Some(RuleId(8)));
    assert_eq!(events[0].http.as_ref().unwrap().method(), "DELETE");
    assert!(!format!("{events:?}").contains("secret"));
}

#[tokio::test]
async fn the_lowest_numbered_rule_that_refuses_is_the_one_named() {
    let (status, _, events) = once(
        "DELETE",
        "/repos/a",
        "",
        vec![
            rule(12, "api.test", &["DELETE"], "/repos/**"),
            rule(3, "api.test", &[], "/repos/a"),
        ],
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(events[0].request_rule, Some(RuleId(3)));
}

#[tokio::test]
async fn a_certificate_alert_on_a_host_with_a_rule_never_teaches_the_host_as_pass_through() {
    let pki = Pki::new();
    let server = api(&pki).await;
    let learned = Arc::new(PassThrough::with_limits(Duration::from_secs(3600), 8));
    let rig = RigBuilder::new(&pki)
        .name("api.test", server.addr)
        .allow(vec!["api.test"])
        .injector(TestInjector::new(|_| {
            puddle_proxy::InjectDecision::PassThrough
        }))
        .decrypting_all(true, &[])
        .learned(Arc::clone(&learned))
        .request_rules(rules(vec![rule(1, "api.test", &["DELETE"], "/**")]))
        .build();

    // A tool whose roots do not hold the workspace CA refuses the certificate.
    let mut guest = rig.guest().await;
    let (port, _bridge) = guest.bridge("api.test:443").await;
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let config = terminate_support::Guest::client_config(std::slice::from_ref(&pki.root), &[]);
    let refused = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(ServerName::try_from("api.test").unwrap(), tcp)
        .await;
    assert!(refused.is_err());
    assert_eq!(
        rig.events(1).await[0].reason.to_string(),
        "guest_tls_rejected"
    );
    assert!(!learned.contains(&host("api.test")));
    assert!(learned.list().is_empty(), "nothing is remembered");

    // The next connection is still decrypted and judged, not passed through.
    let mut client = guest.tls("api.test:443", None).await.unwrap();
    let refused = send(&mut client, "DELETE", "/x", "").await;
    assert_eq!(refused.status, 403);
    client.close().await;
    let events = rig.events(2).await;
    assert_eq!(events[1].reason.to_string(), "request_rule");
    assert!(learned.list().is_empty());
}
