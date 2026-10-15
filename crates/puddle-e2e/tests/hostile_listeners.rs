// SPDX-License-Identifier: GPL-3.0-or-later
//! Hostile-guest tier W, a sweep of the process's own listening sockets: everything Puddle's host
//! side listens on is loopback only, and every such listener is in the endpoint registry, which is
//! what makes the proxy refuse a guest that dials it (so a listener nobody registered would be
//! reachable from a guest as plain loopback).
//!
//! The sweep reads the machine's socket table (`netstat -ano`) and keeps the rows of this process;
//! each test is its own process under nextest, so nothing else of this suite is in it. The control
//! at the end binds a wildcard socket and a loopback one that nobody registered and shows the
//! sweep flags both, so a pass above isn't a probe that sees nothing.
#![cfg(windows)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::collections::BTreeSet;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::Arc;

use puddle_api::{
    ApiConfig, ApiServer, ApiToken, EventHub, MemorySettings, Services, SettingsRepo,
};
use puddle_ipc::IpcRoot;
use puddle_netpolicy::{EndpointKind, LocalAccess, NetPolicy, PuddleEndpoints};
use puddle_proxy::testing::StaticResolver;
use puddle_proxy::{Proxy, PullProxy};
use puddle_store::{Limits, ManualClock, Store};
use puddle_types::{NullSink, WorkspaceName};
use puddle_vm_tests::sockets::{Listening, listening_sockets};

const NOW: u64 = 1_800_000_000_000;

/// This process's listening TCP sockets and bound UDP sockets.
fn own_sockets() -> Vec<Listening> {
    let me = std::process::id();
    let mut mine: Vec<Listening> = listening_sockets()
        .unwrap()
        .into_iter()
        .filter(|s| s.pid == me)
        .collect();
    mine.sort();
    mine
}

/// The sockets that are open to other machines, or that Puddle has not registered.
fn offenders(sockets: &[Listening], registered: &BTreeSet<SocketAddr>) -> Vec<String> {
    sockets
        .iter()
        .filter(|s| {
            let known =
                s.protocol == "TCP" && registered.contains(&SocketAddr::new(s.address, s.port));
            !s.is_loopback() || !known
        })
        .map(ToString::to_string)
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_hg26_every_listener_the_host_side_opens_is_loopback_and_registered() {
    let endpoints = PuddleEndpoints::new();
    let services = Services::new(
        Arc::new(
            Store::open_in_memory(Arc::new(ManualClock::new(NOW)), Limits::default()).unwrap(),
        ),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        Arc::new(EventHub::default()),
        Arc::new(ManualClock::new(NOW)),
    )
    .with_endpoints(endpoints.clone());
    let api = ApiServer::bind(
        ApiConfig::default(),
        ApiToken::generate().unwrap(),
        services,
    )
    .await
    .unwrap()
    .spawn();
    let pull = PullProxy::bind(&endpoints).unwrap().serve().unwrap();
    // A workspace route (a named pipe, so not a socket) and the proxy behind it.
    let store = Arc::new(
        Store::open_in_memory(Arc::new(ManualClock::new(NOW)), Limits::default()).unwrap(),
    );
    let proxy = Arc::new(
        Proxy::new(store, Arc::new(NullSink))
            .with_resolver(Arc::new(StaticResolver::new()))
            .with_address_check(Arc::new(
                NetPolicy::new(Arc::new(LocalAccess::NONE)).with_endpoints(endpoints.clone()),
            )),
    );
    let root = IpcRoot::new().unwrap();
    let _route = proxy.serve_route(root.listen().unwrap(), WorkspaceName::new("hg26").unwrap());

    let registered: BTreeSet<SocketAddr> = endpoints.list().into_iter().map(|(a, _)| a).collect();
    let kinds: Vec<EndpointKind> = endpoints.list().into_iter().map(|(_, k)| k).collect();
    assert!(
        kinds.contains(&EndpointKind::Api) && kinds.contains(&EndpointKind::PullProxy),
        "the rig must register the API and the pull proxy: {kinds:?}"
    );

    let sweep = own_sockets();
    println!("sockets of this process:");
    for socket in &sweep {
        println!("  {socket}");
    }
    // Not vacuous: the sweep sees the two listeners we know are there.
    for known in [api.local_addr(), pull.local_addr()] {
        assert!(
            sweep
                .iter()
                .any(|s| s.protocol == "TCP" && SocketAddr::new(s.address, s.port) == known),
            "the sweep does not list {known}"
        );
    }
    assert_eq!(offenders(&sweep, &registered), Vec::<String>::new());

    // Control: an unregistered loopback listener, a wildcard TCP listener and a wildcard UDP
    // socket are all named as offenders.
    let loose = TcpListener::bind("127.0.0.1:0").unwrap();
    let wide = TcpListener::bind("0.0.0.0:0").unwrap();
    let datagrams = UdpSocket::bind("0.0.0.0:0").unwrap();
    let after = own_sockets();
    let named = offenders(&after, &registered);
    for (what, port) in [
        ("loopback, unregistered", loose.local_addr().unwrap().port()),
        ("wildcard TCP", wide.local_addr().unwrap().port()),
        ("wildcard UDP", datagrams.local_addr().unwrap().port()),
    ] {
        assert!(
            named.iter().any(|n| n.contains(&format!(":{port} "))),
            "the sweep missed the {what} socket on port {port}: {named:?}"
        );
    }
    drop((loose, wide, datagrams));
    api.shutdown().await;
}
