// SPDX-License-Identifier: AGPL-3.0-or-later
//! An image pull behind a company proxy: the adapter's real `pull_image` goes through Puddle's
//! [`PullProxy`], which leaves along the company proxy route to a real Squid with Basic proxy
//! authentication, which tunnels to a TLS registry that only Squid can resolve. Squid's own
//! access log is the witness that the pull went through it as the configured user, and the audit
//! names the hop. A wrong proxy password fails the pull before the registry sees a request.
//!
//! Squid comes from `PUDDLE_SQUID_BIN` or `PUDDLE_SQUID_DOCKER`; with neither the tests skip,
//! unless `PUDDLE_REQUIRE_SQUID=1` (CI sets it).
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use puddle_compute::{ComputeError, Runtime};
use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_e2e::lab::{BASIC_USER, Squid, SquidOptions, basic_password};
use puddle_netpolicy::PuddleEndpoints;
use puddle_proxy::{PullProxy, PullRoute, Upstream};
use puddle_store::{Clock, Limits, Store, SystemClock};
use puddle_types::ImageRef;
use puddle_upstream::{
    Chain, Config as DiscoveryConfig, Discovery, FakeOs, Hop, ProxyAddr, ProxyConfig,
};
use tokio::net::TcpListener;

use crate::lab::registry::{REPO, Seen, registry};
use crate::lab::{self, REGISTRY};
use crate::rig::{basic, connection_records};

/// What one test runs against: the pull runtime, the registry and what each saw.
struct World {
    squid: Squid,
    runtime: MsbRuntime,
    image: ImageRef,
    registry_port: u16,
    seen: Arc<Seen>,
    store: Arc<Store>,
    token: String,
    _home: tempfile::TempDir,
    _route: PullRoute,
}

/// Where the registry will listen, taken before Squid starts: Squid lets a tunnel reach only this port.
fn registry_socket() -> (std::net::TcpListener, SocketAddr) {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    (listener, addr)
}

/// Squid with Basic authentication that resolves only the registry's name, to loopback.
fn squid_options(registry: SocketAddr) -> SquidOptions {
    let mut options = SquidOptions::basic(&[registry.port()]);
    options.names = vec![REGISTRY.to_owned()];
    options
}

/// A pull proxy whose company route is `squid` with the proxy password `password`, in front of a
/// registry on `listener` that only Squid resolves.
async fn world(
    squid: Squid,
    listener: std::net::TcpListener,
    addr: SocketAddr,
    password: &str,
) -> World {
    let (root, tls) = lab::lab_pki();
    let seen = Arc::new(Seen::default());
    tokio::spawn(registry(
        TcpListener::from_std(listener).unwrap(),
        Arc::new(tls),
        Arc::clone(&seen),
    ));

    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/proxy.pac".into()),
        ..ProxyConfig::default()
    });
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", squid.port()));
    os.set_pac(move |_| Ok(vec![hop.clone()]));
    let chain = Chain::new(
        Discovery::new(os, DiscoveryConfig::default()),
        basic(BASIC_USER, password),
    );
    let store = Arc::new(
        Store::open_in_memory(Arc::new(SystemClock) as Arc<dyn Clock>, Limits::default()).unwrap(),
    );
    let proxy = PullProxy::bind(&PuddleEndpoints::new())
        .unwrap()
        .with_resolver(Arc::new(lab::LabResolver(addr)))
        .with_upstream(Upstream::new(chain))
        .with_connection_log(store.clone());
    let token = proxy.token().expose().to_owned();
    let proxy_url = proxy.proxy_url().expose().to_owned();
    let route = proxy.serve().unwrap();
    let home = puddle_test_kit::private_tempdir();
    // Pulling needs no msb binary: the paths only go into msb's config.json.
    let config = MsbConfig::new(
        home.path().join("msb-home"),
        home.path().join("runtime/msb"),
        home.path().join("runtime/libkrunfw"),
        home.path().join("guest-share"),
    )
    .with_registry_proxy(proxy_url)
    .with_registry_roots([root]);
    World {
        squid,
        runtime: MsbRuntime::open(config).await.unwrap(),
        image: ImageRef::new(&format!("{REGISTRY}:{}/{REPO}:1", addr.port())).unwrap(),
        registry_port: addr.port(),
        seen,
        store,
        token,
        _home: home,
        _route: route,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_pull_goes_through_the_company_proxy_as_the_configured_user() {
    let (listener, addr) = registry_socket();
    let Some(squid) = Squid::start(&squid_options(addr)) else {
        return;
    };
    let w = world(squid, listener, addr, &basic_password()).await;
    let pulled = w.runtime.pull_image(&w.image).await.unwrap();
    assert_eq!(pulled.env_var("PUDDLE_LAB"), Some("1"));
    assert_eq!(pulled.cmd, ["/hello"]);
    assert_eq!(w.seen.handshakes_failed.load(Ordering::SeqCst), 0);
    assert!(
        w.seen.requests().iter().any(|r| r.contains("/manifests/")),
        "{:?}",
        w.seen.requests()
    );

    let authority = format!("{REGISTRY}:{}", w.registry_port);
    let log = w.squid.access_log_with("TCP_TUNNEL/200");
    assert!(
        log.contains(&format!("{BASIC_USER} ")) && log.contains(&authority),
        "Squid's access log must show the tunnel for {BASIC_USER}:\n{log}"
    );
    assert!(
        log.contains("TCP_DENIED/407"),
        "the 407 round trip happened:\n{log}"
    );

    let records = connection_records(&w.store, 1).await;
    for record in &records {
        assert_eq!(record["origin"], "puddle", "{record}");
        assert_eq!(record["decision"], "allow", "{record}");
        assert_eq!(
            record["upstream"],
            format!("PROXY 127.0.0.1:{}", w.squid.port()),
            "{record}"
        );
        assert!(!record.to_string().contains(&w.token));
        assert!(!record.to_string().contains(&basic_password()));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_wrong_proxy_password_fails_the_pull_before_the_registry_sees_a_request() {
    let (listener, addr) = registry_socket();
    let Some(squid) = Squid::start(&squid_options(addr)) else {
        return;
    };
    let w = world(
        squid,
        listener,
        addr,
        &format!("{}-but-wrong", basic_password()),
    )
    .await;
    let err = w.runtime.pull_image(&w.image).await.unwrap_err();
    assert!(matches!(err, ComputeError::ImagePull { .. }), "{err:?}");
    assert!(w.seen.requests().is_empty(), "{:?}", w.seen.requests());
    assert_eq!(w.seen.handshakes_failed.load(Ordering::SeqCst), 0);
    let log = w.squid.access_log_with("TCP_DENIED/407");
    assert!(log.contains("TCP_DENIED/407"), "{log}");
    assert!(
        !log.contains("TCP_TUNNEL/200"),
        "no tunnel was opened:\n{log}"
    );
    assert!(!format!("{err:?}").contains(&w.token));
}
