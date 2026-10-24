// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake website: answers every name it is given over plain HTTP and TLS, reached through the
//! fake internet's company proxy, and records what it was sent.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

mod rig;

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
