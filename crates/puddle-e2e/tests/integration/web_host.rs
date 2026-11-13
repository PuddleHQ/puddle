// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fake website: answers every name it is given over plain HTTP and TLS, reached through the
//! fake internet's company proxy, and records what it was sent.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use crate::rig;

use puddle_e2e::lab::{NameProxy, WebHost, WebOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

async fn through(proxy: &NameProxy, first_line: &str, request: &str) -> BufReader<TcpStream> {
    let mut conn = TcpStream::connect(("127.0.0.1", proxy.port()))
        .await
        .unwrap();
    conn.write_all(first_line.as_bytes()).await.unwrap();
    let mut conn = BufReader::new(conn);
    if first_line.starts_with("CONNECT") {
        let mut line = String::new();
        loop {
            line.clear();
            conn.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
        }
    }
    conn.get_mut().write_all(request.as_bytes()).await.unwrap();
    conn
}

#[tokio::test]
async fn a_name_the_site_holds_answers_over_plain_http_and_tls_and_the_site_records_the_requests() {
    let web = WebHost::start(
        &["one.test", "two.test"],
        WebOptions {
            alt_svc: Some("h3=\":443\"; ma=86400".to_owned()),
            ..WebOptions::default()
        },
    )
    .await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, &["one.test", "two.test"]);

    // Plain HTTP, named by an absolute URI (how a company proxy is asked for a plain request).
    let mut conn = through(
        &proxy,
        "GET http://two.test/a?b=1 HTTP/1.1\r\n",
        "Host: two.test\r\nProxy-Authorization: Negotiate abc\r\nX-Probe: plain\r\nConnection: close\r\n\r\n",
    )
    .await;
    let mut text = String::new();
    conn.read_to_string(&mut text).await.unwrap();
    assert!(text.starts_with("HTTP/1.1 200"), "{text}");
    assert!(text.ends_with("lab web host: /a?b=1\n"), "{text}");
    assert!(!text.to_lowercase().contains("alt-svc"), "{text}");

    // TLS through a tunnel, trusting only the site's root: the answer carries the alt-svc header.
    let conn = through(
        &proxy,
        "CONNECT one.test:443 HTTP/1.1\r\nHost: one.test:443\r\n\r\n",
        "",
    )
    .await;
    let answer = rig::https_get_raw(conn, "one.test", &[web.root().as_ref()])
        .await
        .unwrap();
    assert!(answer.ends_with("\r\n\r\nlab web host: /\n"), "{answer}");
    assert!(
        answer
            .to_lowercase()
            .contains("alt-svc: h3=\":443\"; ma=86400"),
        "{answer}"
    );
    let conn = through(
        &proxy,
        "CONNECT one.test:443 HTTP/1.1\r\nHost: one.test:443\r\n\r\n",
        "",
    )
    .await;
    assert!(
        rig::https_get(conn, "one.test", &[]).await.is_err(),
        "a client that does not trust the root must fail"
    );
    assert!(
        !String::from_utf8_lossy(web.root().as_ref()).contains("uddle"),
        "the root's name must not hold the product name"
    );

    let seen = web.seen();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(seen[0].target, "/a?b=1");
    assert_eq!(seen[0].headers_named("x-probe"), ["plain"]);
    assert!(
        seen[0].headers_named("proxy-authorization").is_empty(),
        "a header meant for the proxy reached the site: {seen:?}"
    );
    assert_eq!(seen[1].version, "HTTP/1.1");
    assert_eq!(web.plain_addr().ip(), web.tls_addr().ip());
}

#[tokio::test]
async fn a_probe_of_a_name_that_cannot_exist_is_answered_before_the_real_request_goes_through() {
    let web = WebHost::start(&["one.test"], WebOptions::default()).await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, &["one.test"]);
    let mut conn = through(
        &proxy,
        "HEAD http://puddle.invalid:80/ HTTP/1.1\r\n",
        "Host: puddle.invalid:80\r\n\r\n",
    )
    .await;
    let mut probe = String::new();
    conn.read_line(&mut probe).await.unwrap();
    assert!(probe.starts_with("HTTP/1.1 200"), "{probe}");
    conn.read_line(&mut probe).await.unwrap();
    conn.read_line(&mut probe).await.unwrap();
    conn.get_mut()
        .write_all(
            b"GET http://one.test/after HTTP/1.1\r\nHost: one.test\r\nConnection: close\r\n\r\n",
        )
        .await
        .unwrap();
    let mut text = String::new();
    conn.read_to_string(&mut text).await.unwrap();
    assert!(text.ends_with("lab web host: /after\n"), "{text}");
    assert!(
        !proxy
            .connects()
            .iter()
            .any(|c| c.starts_with("puddle.invalid"))
    );
}

#[tokio::test]
async fn a_name_the_proxy_does_not_hold_is_connected_to_as_it_stands() {
    let proxy = NameProxy::start(&[]).await;
    let elsewhere = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = elsewhere.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut conn, _) = elsewhere.accept().await.unwrap();
        // Read the whole request: closing with unread bytes resets the connection, and on
        // Windows a reset throws away the answer the client has not read yet.
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0_u8; 1];
            conn.read_exact(&mut byte).await.unwrap();
            head.push(byte[0]);
        }
        conn.write_all(b"HTTP/1.1 204 No Content\r\n\r\n")
            .await
            .unwrap();
    });
    let mut conn = through(
        &proxy,
        &format!("GET http://127.0.0.1:{port}/ HTTP/1.1\r\n"),
        "Host: x\r\nConnection: close\r\n\r\n",
    )
    .await;
    let mut text = String::new();
    let read = conn.read_to_string(&mut text).await;
    assert!(
        text.starts_with("HTTP/1.1 204"),
        "read {read:?}, got {text:?}, proxy saw {:?}",
        proxy.connects()
    );
    assert!(proxy.connects().contains(&format!("127.0.0.1:{port}")));
}

#[tokio::test]
async fn a_responder_answers_the_requests_it_wants_and_the_site_records_what_it_answered() {
    let responder: puddle_e2e::lab::Responder = std::sync::Arc::new(|seen| {
        (seen.target == "/token").then(|| puddle_e2e::lab::WebAnswer {
            status: 418,
            content_type: "application/json",
            body: format!("{{\"body\":{}}}", seen.body.len()).into_bytes(),
        })
    });
    let web = WebHost::start(
        &["one.test"],
        WebOptions {
            responder: Some(responder),
            ..WebOptions::default()
        },
    )
    .await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, &["one.test"]);
    let mut conn = through(
        &proxy,
        "POST http://one.test/token HTTP/1.1\r\n",
        "Host: one.test\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
    )
    .await;
    let mut text = String::new();
    conn.read_to_string(&mut text).await.unwrap();
    assert!(text.starts_with("HTTP/1.1 418"), "{text}");
    assert!(text.contains("content-type: application/json"), "{text}");
    assert!(text.ends_with("{\"body\":3}"), "{text}");
    let mut conn = through(
        &proxy,
        "GET http://one.test/other HTTP/1.1\r\n",
        "Host: one.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    let mut other = String::new();
    conn.read_to_string(&mut other).await.unwrap();
    assert!(other.ends_with("lab web host: /other\n"), "{other}");
    let seen = web.seen();
    assert_eq!(
        seen.iter().map(|r| r.status).collect::<Vec<_>>(),
        [418, 200]
    );
    assert!(format!("{:?}", WebOptions::default()).contains("responder"));
}

#[tokio::test]
async fn a_refusal_with_401_carries_a_basic_challenge() {
    let responder: puddle_e2e::lab::Responder = std::sync::Arc::new(|_| {
        Some(puddle_e2e::lab::WebAnswer {
            status: 401,
            content_type: "text/plain",
            body: b"no".to_vec(),
        })
    });
    let web = WebHost::start(
        &["locked.test"],
        WebOptions {
            responder: Some(responder),
            ..WebOptions::default()
        },
    )
    .await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, &["locked.test"]);
    let mut conn = through(
        &proxy,
        "GET http://locked.test/ HTTP/1.1\r\n",
        "Host: locked.test\r\nConnection: close\r\n\r\n",
    )
    .await;
    let mut text = String::new();
    conn.read_to_string(&mut text).await.unwrap();
    assert!(text.starts_with("HTTP/1.1 401"), "{text}");
    assert!(
        text.to_lowercase()
            .contains("www-authenticate: basic realm"),
        "{text}"
    );
}

/// One request through the proxy; the whole answer as text.
async fn ask(proxy: &NameProxy, method: &str, host: &str, path: &str) -> String {
    let mut conn = through(
        proxy,
        &format!("{method} http://{host}{path} HTTP/1.1\r\n"),
        &format!("Host: {host}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"),
    )
    .await;
    let mut text = String::new();
    conn.read_to_string(&mut text).await.unwrap();
    text
}

/// A request path for a profile endpoint's path pattern: a wildcard segment becomes `x`.
fn path_for(pattern: &str) -> String {
    pattern.replace('*', "x")
}

/// The guard against a plain-text token answer: with no responder, every login profile's token endpoint
/// is answered with a type login capture reads, so a VM test (or the next profile's) that reaches
/// one does not stop its login as "another type" (found in the merge queue, not before).
#[tokio::test]
async fn every_login_profiles_token_endpoint_is_answered_with_a_type_capture_reads() {
    let endpoints: Vec<_> = puddle_logins::builtin()
        .iter()
        .flat_map(|profile| profile.endpoints.iter().map(move |e| (profile.id, e)))
        .collect();
    assert!(endpoints.len() >= 6, "{} endpoints", endpoints.len());
    let hosts: Vec<&str> = endpoints.iter().map(|(_, e)| e.host).collect();
    let web = WebHost::start(&hosts, WebOptions::default()).await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, &hosts);
    for (id, endpoint) in endpoints {
        let method = if endpoint.method.accepts("POST") {
            "POST"
        } else {
            "GET"
        };
        let text = ask(&proxy, method, endpoint.host, &path_for(endpoint.path)).await;
        assert!(
            text.starts_with("HTTP/1.1 200"),
            "{id} {}: {text}",
            endpoint.host
        );
        let content_type = text
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-type: ")
                    .map(str::to_owned)
            })
            .unwrap();
        assert!(
            puddle_proxy::TokenBody::reads(Some(&content_type)),
            "{id} {}{}: the default answer is {content_type}, a type capture refuses",
            endpoint.host,
            endpoint.path
        );
    }
    assert_eq!(web.take_unreadable_token_answers(), Vec::<String>::new());
    // A path that is no token endpoint keeps the plain default.
    let other = ask(&proxy, "POST", hosts[0], "/elsewhere").await;
    assert!(other.contains("content-type: text/plain"), "{other}");
}

/// The other half of the guard: a responder that answers a token endpoint with a type capture
/// refuses is named, and only when the answer is a success that capture would read.
#[tokio::test]
async fn a_responder_that_answers_a_token_endpoint_in_a_refused_type_is_named() {
    let responder: puddle_e2e::lab::Responder = std::sync::Arc::new(|seen| {
        let status = match seen.target.as_str() {
            "/v1/oauth/token" => 200,
            "/v1/oauth/token?refused" => 400,
            "/v1/oauth/token?empty" => 204,
            _ => return None,
        };
        Some(puddle_e2e::lab::WebAnswer {
            status,
            content_type: "text/plain",
            body: b"nope".to_vec(),
        })
    });
    let web = WebHost::start(
        &["platform.claude.com", "other.test"],
        WebOptions {
            responder: Some(responder),
            ..WebOptions::default()
        },
    )
    .await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, &["platform.claude.com", "other.test"]);
    for path in ["/v1/oauth/token?refused", "/v1/oauth/token?empty"] {
        ask(&proxy, "POST", "platform.claude.com", path).await;
    }
    ask(&proxy, "POST", "other.test", "/v1/oauth/token").await;
    assert_eq!(web.take_unreadable_token_answers(), Vec::<String>::new());
    ask(&proxy, "POST", "platform.claude.com", "/v1/oauth/token").await;
    assert_eq!(
        web.take_unreadable_token_answers(),
        ["POST platform.claude.com/v1/oauth/token: 200, text/plain"]
    );
    assert_eq!(web.take_unreadable_token_answers(), Vec::<String>::new());
}

#[tokio::test]
#[should_panic(expected = "a fake answered a login token endpoint with a type capture refuses")]
async fn a_site_dropped_with_an_unreadable_token_answer_fails_its_test() {
    let responder: puddle_e2e::lab::Responder = std::sync::Arc::new(|_| {
        Some(puddle_e2e::lab::WebAnswer {
            status: 200,
            content_type: "text/plain",
            body: b"access_token=x".to_vec(),
        })
    });
    let web = WebHost::start(
        &["github.com"],
        WebOptions {
            responder: Some(responder),
            ..WebOptions::default()
        },
    )
    .await;
    let proxy = NameProxy::start(&[]).await;
    web.register(&proxy, &["github.com"]);
    ask(&proxy, "POST", "github.com", "/login/oauth/access_token").await;
    drop(web);
}
