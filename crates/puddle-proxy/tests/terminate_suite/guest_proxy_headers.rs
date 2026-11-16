// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests that a header in Puddle's own `x-puddle-` namespace, written by the guest on a decrypted
//! request, never reaches the real server: on HTTP/1.1 and on HTTP/2 (the plain and pull
//! listeners have the same test beside theirs).
use crate::terminate_support;

use std::sync::Arc;

use http::Version;
use terminate_support::h2_rig::{H2Server, Script, full, reply};
use terminate_support::{FakeServer, Flaw, Pki, Reply, RigBuilder, TestInjector};

/// Spelled in three cases and in a form the guest agent really uses, plus one ordinary header
/// that must still travel.
const OURS: [(&str, &str); 3] = [
    ("x-puddle-workspace", "other"),
    ("X-Puddle-Decision", "allow"),
    ("x-puddle-anything-new", "1"),
];

#[tokio::test]
async fn a_guest_puddle_header_never_reaches_the_real_server_on_http1() {
    let pki = Pki::new();
    let server = FakeServer::tls(
        pki.server_config("bound.test", Flaw::None),
        Arc::new(|_| Reply::ok("ok")),
    )
    .await;
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(TestInjector::new(|_| {
            puddle_proxy::InjectDecision::PassThrough
        }))
        .build();
    let mut guest = rig.guest();
    let mut client = guest.tls("bound.test:443", None).await.unwrap();
    let ours = OURS
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\r\n");
    let head = format!("GET /v1 HTTP/1.1\r\nHost: bound.test\r\nX-Kept: yes\r\n{ours}\r\n\r\n");
    client.send(head.as_bytes()).await;
    assert_eq!(client.response("GET").await.status, 200);
    client.close().await;

    let seen = server.recorded();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].header("x-kept"),
        Some("yes"),
        "other headers travel"
    );
    assert!(
        seen[0]
            .headers
            .iter()
            .all(|(name, _)| !name.to_ascii_lowercase().starts_with("x-puddle-")),
        "{:?}",
        seen[0].headers
    );
}

#[tokio::test]
async fn a_guest_puddle_header_never_reaches_the_real_server_on_http2() {
    let pki = Pki::new();
    let script: Script = Arc::new(|_| reply(200, "ok"));
    let server = H2Server::recording(&pki, "bound.test", script).await;
    let rig = RigBuilder::new(&pki)
        .name("bound.test", server.addr)
        .injector(TestInjector::new(|_| {
            puddle_proxy::InjectDecision::PassThrough
        }))
        .build();
    let mut guest = rig.guest();
    let mut client = guest.h2("bound.test:443", &[b"h2", b"http/1.1"]).await;
    let mut headers = vec![("x-kept", "yes")];
    headers.extend(OURS);
    let got = client
        .request(
            "GET",
            "bound.test",
            "/v1",
            &headers,
            full(bytes::Bytes::new()),
        )
        .await;
    assert_eq!(got.status, 200);
    client.close().await;

    let seen = server.recorded();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].version, Version::HTTP_2);
    assert_eq!(
        seen[0].header("x-kept"),
        Some("yes"),
        "other headers travel"
    );
    assert!(
        seen[0]
            .headers
            .iter()
            .all(|(name, _)| !name.to_ascii_lowercase().starts_with("x-puddle-")),
        "{:?}",
        seen[0].headers
    );
}
