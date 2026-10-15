// SPDX-License-Identifier: GPL-3.0-or-later
//! Windows only: the corporate-lab fixtures against the upstream chain with Windows' own engines.
//! There is no guest agent on Windows, so the chain is driven directly: it is the part of the
//! proxy that picks a route and signs in, and a curl-style client takes the connection it returns.
//!
//! - **S6** the real WinHTTP engine evaluates a PAC served by the lab's PAC server. The list is a
//!   dead proxy, the TLS interceptor, then `DIRECT`: the chain fails over to the interceptor, an
//!   intranet name goes `DIRECT`, and the client trusts the intercepted certificate only with the
//!   interceptor's root.
//! - **S7** the lab's NTLM proxy is signed in to through SSPI as the logged-on user of the runner:
//!   three legs on one connection, then the tunnel carries 64 KiB.
//!
//! The fixtures that need mitmproxy skip without `PUDDLE_MITMDUMP_BIN`, and fail when
//! `PUDDLE_REQUIRE_MITMPROXY` is set (the `windows` workflow sets both). Written without a Windows
//! machine to run on: the first run is the workflow's.
#![cfg(windows)]

mod rig;

use std::net::SocketAddr;
use std::sync::Arc;

use puddle_e2e::lab::{Interceptor, NtlmLeg, NtlmProxy, PacServer, failover_pac};
use puddle_upstream::{
    BasicAuth, Chain, ChangeCallback, Config, Destination, Discovery, FakeOs, Form, Hop, OsProxy,
    PacError, PacQuery, ProblemCallback, ProxyAddr, ProxyConfig, Request, Scheme, SettingsError,
    WatchGuard, WinOs, system_auth,
};
use rig::{HttpsOrigin, echo_server, https_get};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

/// The settings are a PAC URL of this test's own; the PAC is evaluated by the real WinHTTP engine.
/// Nothing in the machine's or the user's settings is read or changed.
#[derive(Debug)]
struct RealPacEngine {
    pac_url: String,
}

impl OsProxy for RealPacEngine {
    fn config(&self) -> Result<ProxyConfig, SettingsError> {
        Ok(ProxyConfig {
            pac_url: Some(self.pac_url.clone()),
            ..ProxyConfig::default()
        })
    }

    fn resolve_pac(&self, query: &PacQuery) -> Result<Vec<Hop>, PacError> {
        WinOs::new().resolve_pac(query)
    }

    fn watch(
        &self,
        _on_change: ChangeCallback,
        _on_problem: ProblemCallback,
    ) -> Option<Box<dyn WatchGuard>> {
        None
    }
}

fn proxy_at(port: u16) -> Hop {
    Hop::Proxy(ProxyAddr::new("127.0.0.1", port))
}

#[tokio::test(flavor = "multi_thread")]
async fn s6_winhttp_fails_over_from_a_dead_proxy_to_the_interceptor_and_sends_the_intranet_direct()
{
    let Some(mitm) = Interceptor::start(&["web.corp.test"]) else {
        return;
    };
    let origin = HttpsOrigin::start("web.corp.test").await;
    let echo = echo_server().await;
    // Bound, never listening: connecting is refused and the port stays ours.
    let dead = tokio::net::TcpSocket::new_v4().unwrap();
    dead.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    let pac = PacServer::start(&failover_pac(
        &["intranet.corp.test"],
        &[
            &format!("127.0.0.1:{dead_port}"),
            &format!("127.0.0.1:{}", mitm.port()),
        ],
    ));
    let chain = Chain::new(
        Discovery::new(
            Arc::new(RealPacEngine { pac_url: pac.url() }),
            Config::default(),
        ),
        Arc::new(BasicAuth::new()),
    );

    let dest = Destination::new(Scheme::Https, "web.corp.test", origin.port);
    let connected = chain
        .connect(&Request::new(&dest, Form::Tunnel, &[]).name_ok(true))
        .await
        .unwrap();
    assert_eq!(
        connected.hop,
        proxy_at(mitm.port()),
        "failed over past the dead hop"
    );
    assert!(
        pac.hits() >= 1,
        "WinHTTP fetched the PAC from the lab server"
    );
    let tunnel = BufReader::new(connected.stream);
    let refused = https_get(tunnel, "web.corp.test", &[&origin.root_der])
        .await
        .unwrap_err();
    assert!(refused.contains("UnknownIssuer"), "{refused}");
    let again = chain
        .connect(&Request::new(&dest, Form::Tunnel, &[]).name_ok(true))
        .await
        .unwrap();
    let body = https_get(
        BufReader::new(again.stream),
        "web.corp.test",
        &[mitm.root().der()],
    )
    .await
    .unwrap();
    assert_eq!(body, HttpsOrigin::BODY, "{}", mitm.log());

    // The intranet name goes DIRECT to the address the guard checked, not through any proxy.
    let intranet = Destination::new(Scheme::Https, "intranet.corp.test", echo.port());
    let checked = [SocketAddr::from(([127, 0, 0, 1], echo.port()))];
    let direct = chain
        .connect(&Request::new(&intranet, Form::Tunnel, &checked))
        .await
        .unwrap();
    assert_eq!(direct.hop, Hop::Direct);
}

#[tokio::test(flavor = "multi_thread")]
async fn s7_sspi_signs_in_to_the_ntlm_proxy_in_three_legs_on_one_connection() {
    let echo = echo_server().await;
    let Some(proxy) = NtlmProxy::start(&["echo.corp.test"]) else {
        return;
    };
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/proxy.pac".into()),
        ..ProxyConfig::default()
    });
    let hop = proxy_at(proxy.port());
    os.set_pac(move |_| Ok(vec![hop.clone()]));
    let chain = Chain::new(Discovery::new(os, Config::default()), system_auth());

    let dest = Destination::new(Scheme::Https, "echo.corp.test", echo.port());
    let connected = chain
        .connect(&Request::new(&dest, Form::Tunnel, &[]).name_ok(true))
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", proxy.log()));
    assert_eq!(connected.hop, proxy_at(proxy.port()));
    let (mut rd, mut wr) = tokio::io::split(connected.stream);
    let payload: Vec<u8> = (0..64 * 1024usize)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    let upload = async {
        wr.write_all(&payload).await.unwrap();
    };
    let download = async {
        let mut back = vec![0_u8; payload.len()];
        rd.read_exact(&mut back).await.unwrap();
        back
    };
    let ((), back) = tokio::join!(upload, download);
    assert_eq!(
        back, payload,
        "64 KiB through the NTLM proxy, byte for byte"
    );

    let legs = proxy.legs();
    let Some(NtlmLeg::Accepted { conn, user, ntlmv2 }) = legs.last().cloned() else {
        panic!(
            "the last leg must be the accepted Type 3: {legs:?}\n{}",
            proxy.log()
        );
    };
    assert!(
        ntlmv2 && user.contains('\\'),
        "an NTLMv2 DOMAIN\\user response: {legs:?}"
    );
    assert!(
        legs.iter()
            .any(|l| matches!(l, NtlmLeg::Challenge { conn: c } if *c == conn)),
        "the challenge and the accept are on one connection: {legs:?}"
    );
}
