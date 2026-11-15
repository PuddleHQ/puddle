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

use std::net::SocketAddr;
use std::sync::atomic::Ordering;

use puddle_compute::{ComputeError, Runtime};
use puddle_e2e::lab::{BASIC_USER, Squid, SquidOptions, basic_password};
use puddle_upstream::{Hop, ProxyAddr};

use crate::lab::REGISTRY;
use crate::lab::pull::{Pull, registry_socket};
use crate::rig::{basic, connection_records};

/// What one test runs against: Squid and the pull world behind it.
struct World {
    squid: Squid,
    pull: Pull,
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
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", squid.port()));
    let pull = Pull::start(hop, basic(BASIC_USER, password), listener, addr, |root| {
        vec![root]
    })
    .await;
    World { squid, pull }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_pull_goes_through_the_company_proxy_as_the_configured_user() {
    let (listener, addr) = registry_socket();
    let Some(squid) = Squid::start(&squid_options(addr)) else {
        return;
    };
    let w = world(squid, listener, addr, &basic_password()).await;
    let pulled = w.pull.runtime.pull_image(&w.pull.image).await.unwrap();
    assert_eq!(pulled.env_var("PUDDLE_LAB"), Some("1"));
    assert_eq!(pulled.cmd, ["/hello"]);
    assert_eq!(w.pull.seen.handshakes_failed.load(Ordering::SeqCst), 0);
    assert!(
        w.pull
            .seen
            .requests()
            .iter()
            .any(|r| r.contains("/manifests/")),
        "{:?}",
        w.pull.seen.requests()
    );

    let authority = format!("{REGISTRY}:{}", w.pull.registry_port);
    let log = w.squid.access_log_with("TCP_TUNNEL/200");
    assert!(
        log.contains(&format!("{BASIC_USER} ")) && log.contains(&authority),
        "Squid's access log must show the tunnel for {BASIC_USER}:\n{log}"
    );
    assert!(
        log.contains("TCP_DENIED/407"),
        "the 407 round trip happened:\n{log}"
    );

    let records = connection_records(&w.pull.store, 1).await;
    for record in &records {
        assert_eq!(record["origin"], "puddle", "{record}");
        assert_eq!(record["decision"], "allow", "{record}");
        assert_eq!(
            record["upstream"],
            format!("PROXY 127.0.0.1:{}", w.squid.port()),
            "{record}"
        );
        assert!(!record.to_string().contains(&w.pull.token));
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
    let err = w.pull.runtime.pull_image(&w.pull.image).await.unwrap_err();
    assert!(matches!(err, ComputeError::ImagePull { .. }), "{err:?}");
    assert!(
        w.pull.seen.requests().is_empty(),
        "{:?}",
        w.pull.seen.requests()
    );
    assert_eq!(w.pull.seen.handshakes_failed.load(Ordering::SeqCst), 0);
    let log = w.squid.access_log_with("TCP_DENIED/407");
    assert!(log.contains("TCP_DENIED/407"), "{log}");
    assert!(
        !log.contains("TCP_TUNNEL/200"),
        "no tunnel was opened:\n{log}"
    );
    assert!(!format!("{err:?}").contains(&w.pull.token));
}
