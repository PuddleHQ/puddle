// SPDX-License-Identifier: AGPL-3.0-or-later
//! One image-pull world: the adapter's real `pull_image` behind Puddle's pull proxy, which leaves
//! along one company-proxy hop to a TLS lab registry that only the hop can resolve.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use puddle_compute_msb::{MsbConfig, MsbRuntime};
use puddle_netpolicy::PuddleEndpoints;
use puddle_proxy::{PullProxy, PullRoute, Upstream};
use puddle_store::{Clock, Limits, Store, SystemClock};
use puddle_types::ImageRef;
use puddle_upstream::{
    Chain, Config as DiscoveryConfig, Discovery, FakeOs, Hop, ProxyAuth, ProxyConfig,
};
use tokio::net::TcpListener;

use super::registry::{REPO, Seen, registry};
use super::{LabResolver, REGISTRY, lab_pki};

/// Where the registry will listen, taken before the proxy starts: a company proxy may let a
/// tunnel reach only this port.
pub(crate) fn registry_socket() -> (std::net::TcpListener, SocketAddr) {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    (listener, addr)
}

/// What one test runs against: the pull runtime, the registry and what each saw.
pub(crate) struct Pull {
    pub(crate) runtime: MsbRuntime,
    pub(crate) image: ImageRef,
    pub(crate) registry_port: u16,
    pub(crate) seen: Arc<Seen>,
    pub(crate) store: Arc<Store>,
    pub(crate) token: String,
    _home: tempfile::TempDir,
    _route: PullRoute,
}

impl Pull {
    /// A pull proxy whose company route is `hop` (answered with `auth`), in front of a registry
    /// on `listener`. `roots` maps the registry's own root (PEM) to the roots the pull client
    /// trusts.
    pub(crate) async fn start(
        hop: Hop,
        auth: Arc<dyn ProxyAuth>,
        listener: std::net::TcpListener,
        addr: SocketAddr,
        roots: impl FnOnce(String) -> Vec<String>,
    ) -> Self {
        let (root, tls) = lab_pki();
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
        os.set_pac(move |_| Ok(vec![hop.clone()]));
        let chain = Chain::new(Discovery::new(os, DiscoveryConfig::default()), auth);
        let store = Arc::new(
            Store::open_in_memory(Arc::new(SystemClock) as Arc<dyn Clock>, Limits::default())
                .unwrap(),
        );
        let proxy = PullProxy::bind(&PuddleEndpoints::new())
            .unwrap()
            .with_resolver(Arc::new(LabResolver(addr)))
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
        .with_registry_roots(roots(root));
        Self {
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
}
