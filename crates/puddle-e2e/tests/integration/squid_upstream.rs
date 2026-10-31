// SPDX-License-Identifier: GPL-3.0-or-later
//! L2 end to end against a real Squid with Basic proxy authentication: a guest
//! TCP client → the real guest agent → yamux over the workspace's real endpoint → the real proxy
//! (real SQLite rules and audit) → upstream chaining (route from a PAC, dead hop, `407` Basic) →
//! Squid → a local server. The audit must name the hop, and Squid's own access log is the witness
//! that traffic went through it as the configured user.
//!
//! Squid is started from `PUDDLE_SQUID_BIN` (a `squid` binary, as on the CI runner) or
//! `PUDDLE_SQUID_DOCKER` (an image such as `ubuntu/squid`, run with host networking, as in a
//! local lab). With neither set the tests skip, unless `PUDDLE_REQUIRE_SQUID=1`, which makes a
//! missing Squid a failure (CI sets it).
#![cfg(unix)]

use crate::rig;

use std::sync::Arc;

use puddle_e2e::lab::{BASIC_USER as USER, Squid, SquidOptions, basic_password};
use puddle_upstream::{BasicAuth, Hop, ProxyAddr, ProxyAuth};
use rig::{
    allow, basic, connection_records, dead_proxy, echo_roundtrip, echo_server, rig,
    status_and_body, tunnel,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use rig::LOCAL;

fn squid_addr(squid: &Squid) -> ProxyAddr {
    ProxyAddr::new("127.0.0.1", squid.port())
}

#[tokio::test]
async fn a_tunnel_goes_through_basic_auth_squid_after_a_dead_pac_hop() {
    let echo = echo_server().await;
    let Some(squid) = Squid::start(&SquidOptions::basic(&[echo.port()])) else {
        return;
    };
    let (dead, _held) = dead_proxy();
    let rig = rig(
        vec![
            Hop::Proxy(dead),
            Hop::Proxy(squid_addr(&squid)),
            Hop::Direct,
        ],
        basic(USER, &basic_password()),
    )
    .await;
    allow(&rig.store, "echo.corp.test");
    let authority = format!("echo.corp.test:{}", echo.port());
    let reader = tunnel(rig.agent.local_addr(), &authority).await;
    let (payload, back) = echo_roundtrip(reader).await;
    assert_eq!(back, payload, "64 KiB through Squid, byte for byte");

    let records = connection_records(&rig.store, 1).await;
    assert_eq!(records[0]["decision"], "allow");
    assert_eq!(
        records[0]["upstream"],
        format!("PROXY {}", squid_addr(&squid))
    );
    let log = squid.access_log_with("TCP_TUNNEL/200");
    assert!(
        log.contains("TCP_TUNNEL/200")
            && log.contains(&format!("{USER} "))
            && log.contains(&authority),
        "Squid's access log must show the tunnel for {USER}:\n{log}"
    );
    assert!(
        log.contains("TCP_DENIED/407"),
        "the 407 round trip happened:\n{log}"
    );
}

#[tokio::test]
async fn plain_http_goes_through_squid_with_the_basic_credential() {
    let origin = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let origin_addr = origin.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (conn, _) = origin.accept().await.unwrap();
            tokio::spawn(async move {
                let mut reader = BufReader::new(conn);
                loop {
                    let mut l = String::new();
                    if reader.read_line(&mut l).await.unwrap_or(0) == 0 || l == "\r\n" {
                        break;
                    }
                }
                let _ = reader
                    .get_mut()
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 21\r\nconnection: close\r\n\r\nhello from the origin")
                    .await;
                let _ = reader.get_mut().shutdown().await;
            });
        }
    });
    let Some(squid) = Squid::start(&SquidOptions::basic(&[origin_addr.port()])) else {
        return;
    };
    let rig = rig(
        vec![Hop::Proxy(squid_addr(&squid))],
        basic(USER, &basic_password()),
    )
    .await;
    allow(&rig.store, "web.corp.test");
    let (code, _, body) = status_and_body(
        rig.agent.local_addr(),
        &format!(
            "GET http://web.corp.test:{}/hello HTTP/1.1\r\nHost: web.corp.test\r\n\r\n",
            origin_addr.port()
        ),
    )
    .await;
    assert_eq!((code, body.as_str()), (200, "hello from the origin"));
    let records = connection_records(&rig.store, 1).await;
    assert_eq!(
        records[0]["upstream"],
        format!("PROXY {}", squid_addr(&squid))
    );
    assert_eq!(records[0]["method"], "GET");
    let log = squid.access_log_with("TCP_MISS/200");
    assert!(
        log.contains("TCP_MISS/200") && log.contains(&format!("{USER} ")) && log.contains("/hello"),
        "{log}"
    );
}

#[tokio::test]
async fn a_wrong_or_missing_password_is_a_502_naming_upstream_auth_and_squid_never_tunnels() {
    let echo = echo_server().await;
    let Some(squid) = Squid::start(&SquidOptions::basic(&[echo.port()])) else {
        return;
    };
    for auth in [
        basic(USER, &puddle_types::fixture::password("not-the")),
        Arc::new(BasicAuth::new()) as Arc<dyn ProxyAuth>,
    ] {
        let rig = rig(vec![Hop::Proxy(squid_addr(&squid)), Hop::Direct], auth).await;
        allow(&rig.store, "echo.corp.test");
        let (code, headers, body) = status_and_body(
            rig.agent.local_addr(),
            &format!(
                "CONNECT echo.corp.test:{port} HTTP/1.1\r\nHost: echo.corp.test:{port}\r\n\r\n",
                port = echo.port()
            ),
        )
        .await;
        assert_eq!(code, 502, "{body}");
        assert!(
            headers
                .iter()
                .any(|h| h == "x-puddle-blocked-by: upstream-auth"),
            "{headers:?}"
        );
        assert!(!body.contains("not-the-password"));
    }
    let log = squid.access_log_with("TCP_DENIED/407");
    assert!(log.contains("TCP_DENIED/407"), "{log}");
    assert!(
        !log.contains("TCP_TUNNEL"),
        "no tunnel without a valid login:\n{log}"
    );
}
