// SPDX-License-Identifier: AGPL-3.0-or-later
//! An image pull behind a TLS-intercepting company proxy: the adapter's real `pull_image` goes
//! through Puddle's [`PullProxy`], which leaves along the company route to mitmproxy, which
//! terminates the registry's TLS and re-signs it with a root made for the run. The registry's
//! name resolves only inside mitmproxy.
//!
//! - With the interceptor's root in the pull client's trust, the image is pulled and the
//!   interceptor's log shows the decrypted registry requests.
//! - Without it, the pull fails on the certificate and the registry sees no HTTP request.
//!
//! mitmproxy comes from `PUDDLE_MITMDUMP_BIN`; without it the tests skip, unless
//! `PUDDLE_REQUIRE_MITMPROXY=1` (CI sets it).
#![cfg(unix)]

use std::sync::Arc;

use puddle_compute::{ComputeError, Runtime};
use puddle_e2e::lab::Interceptor;
use puddle_upstream::{Hop, NoAuth, ProxyAddr};

use crate::lab::REGISTRY;
use crate::lab::pull::{Pull, registry_socket};
use crate::rig::connection_records;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_pull_through_an_intercepting_proxy_works_with_its_root_trusted() {
    let (listener, addr) = registry_socket();
    let Some(mitm) = Interceptor::start_on_ports(&[(REGISTRY.to_owned(), Some(addr.port()))])
    else {
        // skip-ok: Interceptor::start_on_ports panics when PUDDLE_REQUIRE_MITMPROXY is set and mitmproxy is missing.
        return;
    };
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", mitm.port()));
    let interceptor_root = mitm.root().pem().to_owned();
    let pull = Pull::start(hop, Arc::new(NoAuth), listener, addr, |_| {
        vec![interceptor_root]
    })
    .await;

    let pulled = pull.runtime.pull_image(&pull.image).await.unwrap();
    assert_eq!(pulled.env_var("PUDDLE_LAB"), Some("1"), "{}", mitm.log());
    assert_eq!(pulled.cmd, ["/hello"]);
    assert!(
        pull.seen
            .requests()
            .iter()
            .any(|r| r.contains("/manifests/")),
        "{:?}",
        pull.seen.requests()
    );

    let log = mitm.log();
    assert!(
        log.contains(&format!("GET https://127.0.0.1:{}/v2/", addr.port()))
            && log.contains("/manifests/")
            && log.contains("200 OK"),
        "the interceptor decrypted the pull:\n{log}"
    );
    let records = connection_records(&pull.store, 1).await;
    for record in &records {
        assert_eq!(
            record["upstream"],
            format!("PROXY 127.0.0.1:{}", mitm.port()),
            "{record}"
        );
        assert!(!record.to_string().contains(&pull.token));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_image_pull_through_an_intercepting_proxy_fails_on_the_certificate_without_its_root() {
    let (listener, addr) = registry_socket();
    let Some(mitm) = Interceptor::start_on_ports(&[(REGISTRY.to_owned(), Some(addr.port()))])
    else {
        // skip-ok: Interceptor::start_on_ports panics when PUDDLE_REQUIRE_MITMPROXY is set and mitmproxy is missing.
        return;
    };
    let hop = Hop::Proxy(ProxyAddr::new("127.0.0.1", mitm.port()));
    // The client trusts only the registry's own root, which the re-signed certificate does not
    // chain to.
    let pull = Pull::start(hop, Arc::new(NoAuth), listener, addr, |lab_root| {
        vec![lab_root]
    })
    .await;

    let err = pull.runtime.pull_image(&pull.image).await.unwrap_err();
    assert!(matches!(err, ComputeError::ImagePull { .. }), "{err:?}");
    assert!(
        format!("{err:?}").contains("UnknownIssuer"),
        "the failure must be the issuer: {err:?}"
    );
    assert!(
        pull.seen.requests().is_empty(),
        "no HTTP request got through: {:?}",
        pull.seen.requests()
    );
    assert!(!format!("{err:?}").contains(&pull.token));
}
