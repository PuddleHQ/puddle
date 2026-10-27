// SPDX-License-Identifier: GPL-3.0-or-later
//! A network change while a tunnel is open, against the lab's Squid: the machine leaves the
//! company network (the route goes from the proxy to `DIRECT`) and comes back. A tunnel on the
//! route that is gone is closed within two seconds so its client reconnects, and new connections
//! take the new route at once. The route flips through a scripted OS layer: the Linux guest has
//! no PAC engine, and what is under test is what Puddle does when the answer changes.
//!
//! Skips without Squid and fails when `PUDDLE_REQUIRE_SQUID` is set (CI sets it).
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use crate::rig;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use puddle_e2e::lab::{NtlmLeg, NtlmProxy, Squid, SquidOptions};
use puddle_proxy::testing::StaticResolver;
use puddle_upstream::{
    BasicAuth, Config, Discovery, FakeOs, Hop, NegotiateAuth, ProxyAddr, ProxyAuth, ProxyConfig,
};
use rig::{
    HandBuiltNtlmSource, LOCAL, allow, connection_records, echo_roundtrip, echo_server, rig_with,
    try_tunnel, tunnel,
};
use tokio::io::{AsyncReadExt, BufReader};
use tokio::net::TcpStream;

/// A route the test can change: the scripted PAC answers what `answer` holds, and `flip` starts
/// the new network epoch the OS watcher starts after its quiet period.
struct Network {
    discovery: Arc<Discovery>,
    answer: Arc<Mutex<Vec<Hop>>>,
}

impl Network {
    fn new(hops: Vec<Hop>) -> Self {
        let os = FakeOs::new(ProxyConfig {
            pac_url: Some("http://pac.corp/proxy.pac".into()),
            ..ProxyConfig::default()
        });
        let answer = Arc::new(Mutex::new(hops));
        let current = Arc::clone(&answer);
        os.set_pac(move |_| Ok(current.lock().unwrap().clone()));
        Self {
            discovery: Discovery::new(os, Config::default()),
            answer,
        }
    }

    fn flip(&self, hops: Vec<Hop>) {
        *self.answer.lock().unwrap() = hops;
        self.discovery.bump_epoch();
    }
}

/// How long the tunnel takes to end once it has been told the network changed.
async fn closes_within(reader: &mut BufReader<TcpStream>, limit: Duration) -> Option<Duration> {
    let started = Instant::now();
    let mut buf = [0_u8; 64];
    tokio::time::timeout(limit, async {
        loop {
            match reader.read(&mut buf).await {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await
    .ok()
    .map(|()| started.elapsed())
}

fn no_auth() -> Arc<dyn ProxyAuth> {
    Arc::new(BasicAuth::new())
}

#[tokio::test]
async fn a_tunnel_through_squid_is_closed_when_the_machine_goes_direct_and_new_ones_follow_the_route()
 {
    let echo = echo_server().await;
    let Some(squid) = Squid::start(&SquidOptions::open(&[echo.port()])) else {
        return;
    };
    let proxy = Hop::Proxy(ProxyAddr::new("127.0.0.1", squid.port()));
    let network = Network::new(vec![proxy.clone()]);
    let resolver = StaticResolver::new().with("echo.corp.test", &[std::net::IpAddr::V4(LOCAL)]);
    let rig = rig_with(Arc::clone(&network.discovery), no_auth(), resolver).await;
    allow(&rig.store, "echo.corp.test");
    let authority = format!("echo.corp.test:{}", echo.port());

    // On the company network: the tunnel goes through Squid.
    let mut on_company = tunnel(rig.agent.local_addr(), &authority).await;
    echo_roundtrip_on(&mut on_company).await;

    // Off it: the route is DIRECT. The old tunnel is closed, and the next one does not touch Squid.
    network.flip(vec![Hop::Direct]);
    closes_within(&mut on_company, Duration::from_secs(2))
        .await
        .unwrap_or_else(|| panic!("the tunnel must close within 2 s\n{}", squid.access_log()));
    // Squid logs a tunnel when it ends: the closed one is there now, through the proxy.
    wait_for_tunnels(&squid, 1).await;
    let tunnels_before = squid.access_log().matches("TCP_TUNNEL/200").count();
    let direct = tunnel(rig.agent.local_addr(), &authority).await;
    let (sent, back) = echo_roundtrip(direct).await;
    assert_eq!(back, sent, "64 KiB direct, byte for byte");
    assert_eq!(
        squid.access_log().matches("TCP_TUNNEL/200").count(),
        tunnels_before,
        "the direct tunnel did not go through Squid:\n{}",
        squid.access_log()
    );

    // Back on the company network: DIRECT is gone from the route, so that tunnel closes too, and
    // the next one is through Squid again.
    let mut off_company = tunnel(rig.agent.local_addr(), &authority).await;
    echo_roundtrip_on(&mut off_company).await;
    network.flip(vec![proxy]);
    assert!(
        closes_within(&mut off_company, Duration::from_secs(2))
            .await
            .is_some(),
        "the direct tunnel must close within 2 s"
    );
    let again = tunnel(rig.agent.local_addr(), &authority).await;
    let (sent, back) = echo_roundtrip(again).await;
    assert_eq!(back, sent);
    wait_for_tunnels(&squid, tunnels_before + 1).await;

    let hops: Vec<String> = connection_records(&rig.store, 4)
        .await
        .iter()
        .map(|r| r["upstream"].as_str().unwrap_or_default().to_owned())
        .collect();
    let via = format!("PROXY 127.0.0.1:{}", squid.port());
    assert_eq!(
        hops.iter().filter(|h| **h == via).count(),
        2,
        "two tunnels through Squid: {hops:?}"
    );
    assert_eq!(
        hops.iter().filter(|h| *h == "DIRECT").count(),
        2,
        "two direct: {hops:?}"
    );
}

/// Waits until Squid has logged `count` finished tunnels (it logs a tunnel when it ends).
async fn wait_for_tunnels(squid: &Squid, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while squid.access_log().matches("TCP_TUNNEL/200").count() < count && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let log = squid.access_log();
    assert!(
        log.matches("TCP_TUNNEL/200").count() >= count,
        "{count} tunnels expected:\n{log}"
    );
}

/// One small echo on a tunnel that stays open for the test.
async fn echo_roundtrip_on(reader: &mut BufReader<TcpStream>) {
    use tokio::io::AsyncWriteExt;
    reader.write_all(b"still here").await.unwrap();
    let mut back = [0_u8; 10];
    reader.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"still here");
}

#[tokio::test]
async fn an_ntlm_proxy_is_asked_from_the_start_again_after_the_network_changes() {
    let echo = echo_server().await;
    let Some(proxy) = NtlmProxy::start(&["echo.corp.test"]) else {
        return;
    };
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", proxy.port()));
    let network = Network::new(vec![hop.clone()]);
    let auth = Arc::new(NegotiateAuth::new(Arc::new(HandBuiltNtlmSource {
        domain: "LAB",
        user: "alice",
    })));
    let rig = rig_with(Arc::clone(&network.discovery), auth, StaticResolver::new()).await;
    allow(&rig.store, "echo.corp.test");
    let authority = format!("echo.corp.test:{}", echo.port());
    let connect = || async {
        let reader = try_tunnel(rig.agent.local_addr(), &authority)
            .await
            .unwrap_or_else(|e| panic!("{e}\n{}", proxy.log()));
        let (sent, back) = echo_roundtrip(reader).await;
        assert_eq!(back, sent);
    };
    let demands = || {
        proxy
            .legs()
            .iter()
            .filter(|leg| matches!(leg, NtlmLeg::Demand { .. }))
            .count()
    };

    // The first connection learns what the proxy asks for; the second goes straight to it.
    connect().await;
    let after_first = demands();
    connect().await;
    assert_eq!(demands(), after_first, "{:?}", proxy.legs());

    // A new network: the proxy behind that address may be another one, so it is asked afresh.
    network.flip(vec![hop]);
    connect().await;
    assert!(
        demands() > after_first,
        "the first connection of a new epoch starts without what was learned: {:?}",
        proxy.legs()
    );
}
