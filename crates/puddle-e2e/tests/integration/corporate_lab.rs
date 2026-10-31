// SPDX-License-Identifier: GPL-3.0-or-later
//! The corporate-lab fixtures against the sandbox proxy: a guest TCP client → the real guest agent →
//! the real proxy (SQLite rules and audit) → upstream chaining → a lab fixture → a local origin.
//!
//! - **S2** an explicit proxy without authentication (open Squid): the tunnel goes through it, and
//!   a name only the proxy resolves (split DNS) works.
//! - **S5** a TLS-intercepting proxy with a root made for the run: without that root in the
//!   client's trust the handshake fails, with it the request is answered, and the interceptor's
//!   log shows the decrypted request.
//! - **Fixtures**: each starts in under ten seconds of its own CPU time on a free port with a
//!   per-run prefix.
//!
//! The Basic-auth proxy (S4) is `squid_upstream.rs`; the PAC and NTLM scenarios (S6, S7) need
//! Windows' engines and are `corporate_lab_windows.rs`. Fixtures skip without their program and
//! fail when `PUDDLE_REQUIRE_SQUID` / `PUDDLE_REQUIRE_MITMPROXY` is set (CI sets both).
#![cfg(unix)]

use crate::rig;

use std::sync::Arc;
use std::time::Duration;

use puddle_e2e::lab::NtlmLeg;
use puddle_e2e::lab::{Interceptor, NtlmProxy, PacServer, Squid, SquidOptions, failover_pac};
use puddle_upstream::{BasicAuth, Hop, NegotiateAuth, ProxyAddr, ProxyAuth};
use rig::{
    HandBuiltNtlmSource, HttpsOrigin, allow, connection_records, echo_roundtrip, echo_server,
    https_get, rig, try_tunnel, tunnel,
};

fn no_auth() -> Arc<dyn ProxyAuth> {
    Arc::new(BasicAuth::new())
}

fn distinct<T: Ord>(v: &[T]) -> bool {
    v.iter().collect::<std::collections::BTreeSet<_>>().len() == v.len()
}

fn proxy_at(port: u16) -> Hop {
    Hop::Proxy(ProxyAddr::new("127.0.0.1", port))
}

#[tokio::test]
async fn s2_an_open_explicit_proxy_carries_a_tunnel_to_a_name_only_it_resolves() {
    let echo = echo_server().await;
    let Some(squid) = Squid::start(&SquidOptions::open(&[echo.port()])) else {
        return;
    };
    let rig = rig(vec![proxy_at(squid.port()), Hop::Direct], no_auth()).await;
    allow(&rig.store, "echo.corp.test");
    let authority = format!("echo.corp.test:{}", echo.port());
    let reader = tunnel(rig.agent.local_addr(), &authority).await;
    let (sent, back) = echo_roundtrip(reader).await;
    assert_eq!(back, sent, "64 KiB through the open proxy, byte for byte");

    let records = connection_records(&rig.store, 1).await;
    assert_eq!(
        records[0]["upstream"],
        format!("PROXY 127.0.0.1:{}", squid.port())
    );
    let log = squid.access_log();
    assert!(
        log.contains("TCP_TUNNEL/200") && log.contains(&authority),
        "{log}"
    );
    assert!(
        !log.contains("TCP_DENIED"),
        "an open proxy never asks for a login:\n{log}"
    );
}

#[tokio::test]
async fn s5_an_intercepting_proxy_is_trusted_only_with_its_root() {
    let Some(mitm) = Interceptor::start(&["web.corp.test"]) else {
        return;
    };
    let origin = HttpsOrigin::start("web.corp.test").await;
    let rig = rig(vec![proxy_at(mitm.port())], no_auth()).await;
    allow(&rig.store, "web.corp.test");
    let authority = format!("web.corp.test:{}", origin.port);

    // The root is not synced: the client trusts the origin's own root, which the intercepted
    // certificate does not chain to.
    let tunnel_without = try_tunnel(rig.agent.local_addr(), &authority)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", mitm.log()));
    let refused = https_get(tunnel_without, "web.corp.test", &[&origin.root_der])
        .await
        .unwrap_err();
    assert!(
        refused.contains("UnknownIssuer"),
        "the handshake must fail on the issuer: {refused}"
    );

    // The root is synced: the client trusts the interceptor's root and is answered.
    let tunnel_with = tunnel(rig.agent.local_addr(), &authority).await;
    let body = https_get(tunnel_with, "web.corp.test", &[mitm.root().der()])
        .await
        .unwrap();
    assert_eq!(body, HttpsOrigin::BODY, "{}", mitm.log());

    // The interceptor's own log is the witness: it refused to be trusted the first time, and
    // decrypted the request the second (its split-DNS step shows the origin by address).
    let log = mitm.log();
    assert!(
        log.contains("does not trust the proxy's certificate for web.corp.test"),
        "{log}"
    );
    assert!(
        log.contains(&format!("GET https://127.0.0.1:{}/", origin.port)) && log.contains("200 OK"),
        "the interceptor decrypted the request:\n{log}"
    );
    let records = connection_records(&rig.store, 2).await;
    assert!(
        records
            .iter()
            .all(|r| r["upstream"] == format!("PROXY 127.0.0.1:{}", mitm.port())),
        "{records:?}"
    );
}

#[tokio::test]
async fn every_fixture_starts_in_under_ten_seconds_on_its_own_free_port_with_a_run_prefix() {
    let started = [
        Squid::start(&SquidOptions::open(&[1]))
            .map(|s| (s.port(), s.prefix().clone(), s.start_cost())),
        Squid::start(&SquidOptions::basic(&[1]))
            .map(|s| (s.port(), s.prefix().clone(), s.start_cost())),
        Interceptor::start(&["web.corp.test"])
            .map(|m| (m.port(), m.prefix().clone(), m.start_cost())),
        NtlmProxy::start(&["echo.corp.test"])
            .map(|n| (n.port(), n.prefix().clone(), n.start_cost())),
    ];
    let mut ports = Vec::new();
    let mut prefixes = Vec::new();
    for (port, prefix, cost) in started.into_iter().flatten() {
        // CPU time, not wall time: see `StartCost`. A start that never completes fails at `READY`.
        // flake-ok: `bounded()` is the fixture child's CPU time (the CI runner starts Squid from a
        // binary, so it has one); load stretches the wall clock, not the CPU a start spends.
        assert!(
            cost.bounded() < Duration::from_secs(10),
            "{} took {cost:?}",
            prefix.as_str()
        );
        assert!(
            prefix.as_str().starts_with("puddle-lab-"),
            "{}",
            prefix.as_str()
        );
        assert!(
            prefix.as_str().contains(&std::process::id().to_string()),
            "{}",
            prefix.as_str()
        );
        ports.push(port);
        prefixes.push(prefix.as_str().to_owned());
    }
    assert!(distinct(&ports), "ports collide: {ports:?}");
    assert!(distinct(&prefixes), "prefixes collide: {prefixes:?}");
}

#[test]
fn the_pac_server_hands_out_its_failover_list_and_counts_fetches() {
    use std::io::{Read, Write};
    let pac = failover_pac(&[".corp.test"], &["127.0.0.1:1", "127.0.0.1:2"]);
    let server = PacServer::start(&pac);
    let url = server.url();
    let (addr, path) = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split_once('/'))
        .unwrap();
    let other = PacServer::start(&pac).url();
    assert!(
        !other.ends_with(path),
        "every server has a path of its own: {other}"
    );
    let mut conn = std::net::TcpStream::connect(addr).unwrap();
    conn.write_all(format!("GET /{path} HTTP/1.1\r\nHost: lab\r\n\r\n").as_bytes())
        .unwrap();
    let mut response = String::new();
    conn.read_to_string(&mut response).unwrap();
    assert!(
        response.contains("application/x-ns-proxy-autoconfig")
            && response.contains("return \"PROXY 127.0.0.1:1; PROXY 127.0.0.1:2; DIRECT\";")
            && response.contains("dnsDomainIs(host, \".corp.test\")"),
        "{response}"
    );
    assert_eq!(server.hits(), 1);
}

#[tokio::test]
async fn s7_an_ntlm_proxy_gets_three_legs_on_one_connection_before_it_tunnels() {
    let echo = echo_server().await;
    let Some(proxy) = NtlmProxy::start(&["echo.corp.test"]) else {
        return;
    };
    let auth = Arc::new(NegotiateAuth::new(Arc::new(HandBuiltNtlmSource {
        domain: "LAB",
        user: "alice",
    })));
    let rig = rig(vec![proxy_at(proxy.port())], auth).await;
    allow(&rig.store, "echo.corp.test");
    let authority = format!("echo.corp.test:{}", echo.port());
    let reader = try_tunnel(rig.agent.local_addr(), &authority)
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", proxy.log()));
    let (sent, back) = echo_roundtrip(reader).await;
    assert_eq!(back, sent, "64 KiB through the NTLM proxy, byte for byte");

    assert!(
        proxy.log().contains("NTLM"),
        "the proxy's log records the handshake"
    );
    let legs = proxy.legs();
    let conn = match legs.last() {
        Some(NtlmLeg::Accepted { conn, .. }) => conn.clone(),
        other => panic!(
            "the last leg must be the accepted Type 3: {other:?}\n{}",
            proxy.log()
        ),
    };
    assert!(
        matches!(
            legs.iter()
                .filter(|l| leg_conn(l) == conn)
                .collect::<Vec<_>>()[..],
            [
                NtlmLeg::Demand { .. } | NtlmLeg::Challenge { .. },
                ..,
                NtlmLeg::Accepted { .. }
            ]
        ) && legs
            .iter()
            .any(|l| matches!(l, NtlmLeg::Challenge { conn: c } if *c == conn)),
        "challenge and accept share one client connection: {legs:?}"
    );
    assert_eq!(
        legs.last(),
        Some(&NtlmLeg::Accepted {
            conn,
            user: "LAB\\alice".into(),
            ntlmv2: true
        })
    );
    let records = connection_records(&rig.store, 1).await;
    assert_eq!(
        records[0]["upstream"],
        format!("PROXY 127.0.0.1:{}", proxy.port())
    );
}

fn leg_conn(leg: &NtlmLeg) -> String {
    match leg {
        NtlmLeg::Demand { conn } | NtlmLeg::Challenge { conn } | NtlmLeg::Accepted { conn, .. } => {
            conn.clone()
        }
    }
}
