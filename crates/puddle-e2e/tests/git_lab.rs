// SPDX-License-Identifier: GPL-3.0-or-later
//! Git through a corporate lab: the real `git`, the real agent, proxy and injector, and between
//! the proxy and the fake Git host a real Squid that asks for a Basic sign-in. The fake host's name
//! is one only the company proxy side knows; Squid hands every tunnel to its parent, a small proxy
//! that maps the name to the fake host's port (a decrypted host is always on 443, which a lab
//! cannot bind).
//!
//! Needs `git` and Squid (`PUDDLE_SQUID_BIN` or `PUDDLE_SQUID_DOCKER`); without them the tests
//! skip unless `PUDDLE_REQUIRE_TOOLS` and `PUDDLE_REQUIRE_SQUID` are set (CI sets them).
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
mod rig;

use std::sync::Arc;

use puddle_e2e::lab::{
    GitHost, Interceptor, NameProxy, NtlmLeg, NtlmProxy, Squid, SquidOptions, Versions,
    git_available,
};
use puddle_upstream::{BasicAuth, Credentials, Hop, NegotiateAuth, ProxyAddr};
use rig::{GitRig, HandBuiltNtlmSource, NAME};

#[tokio::test]
async fn git_clones_and_pushes_through_a_squid_that_asks_for_a_basic_sign_in() {
    if !git_available() {
        return;
    }
    // Squid needs the parent proxy's port and the parent needs the fake host's address, which
    // the rig makes: the parent starts empty and is told the name afterwards.
    let parent = NameProxy::start(&[]).await;
    let Some(squid) = Squid::start(&SquidOptions::basic(&[443]).through(parent.port())) else {
        return;
    };
    let auth = Arc::new(BasicAuth::new().with_default(Credentials::new(
        puddle_e2e::lab::BASIC_USER,
        puddle_e2e::lab::basic_password(),
    )));
    let rig = GitRig::via(
        vec![Hop::Proxy(ProxyAddr::new("127.0.0.1", squid.port()))],
        auth,
    )
    .await;
    parent.route(NAME, rig.host.addr());

    let dir = rig.work("w");
    let url = format!("https://{NAME}/acme/web.git");
    let ran = rig.run(&dir, &["clone", "-q", &url, "web"]).await;
    assert!(ran.ok, "{}\n{}", ran.stderr, squid.access_log());
    let clone = dir.join("web");
    std::fs::write(clone.join("lab.txt"), "through squid\n").unwrap();
    for args in [&["add", "lab.txt"][..], &["commit", "-q", "-m", "lab"]] {
        let ran = rig.run(&clone, args).await;
        assert!(ran.ok, "{args:?}: {}", ran.stderr);
    }
    let ran = rig.run(&clone, &["push", "-q", "origin", "main"]).await;
    assert!(ran.ok, "{}\n{}", ran.stderr, squid.access_log());

    // The host saw the work identity's credential on every request, and git sent none.
    let seen = rig.host.seen();
    assert!(seen.len() >= 4, "{seen:#?}");
    for request in &seen {
        assert_eq!(
            request.headers_named("authorization").len(),
            1,
            "{} {}",
            request.method,
            request.target
        );
        assert!(request.headers_named("authorization")[0].starts_with("Basic "));
    }
    assert!(
        seen.iter().any(|r| r.target.contains("git-receive-pack")),
        "the push reached the host"
    );
    // Squid carried the tunnel, signed in as the lab user, and its parent saw the real name.
    let log = squid.access_log();
    assert!(
        log.contains("TCP_TUNNEL/200")
            && log.contains(&format!("{NAME}:443"))
            && log.contains("alice"),
        "{log}"
    );
    assert!(
        parent
            .connects()
            .iter()
            .all(|c| c == &format!("{NAME}:443")),
        "{:?}",
        parent.connects()
    );
    // The audit record names the company proxy hop.
    let records = rig.records(1).await;
    assert!(
        records
            .iter()
            .any(|r| r["upstream"] == format!("PROXY 127.0.0.1:{}", squid.port())),
        "{records:#?}"
    );
}

#[tokio::test]
async fn a_wrong_basic_sign_in_stops_the_clone_before_anything_reaches_the_host() {
    if !git_available() {
        return;
    }
    let parent = NameProxy::start(&[]).await;
    let Some(squid) = Squid::start(&SquidOptions::basic(&[443]).through(parent.port())) else {
        return;
    };
    let auth = Arc::new(BasicAuth::new().with_default(Credentials::new(
        puddle_e2e::lab::BASIC_USER,
        puddle_types::fixture::password("not-the-lab"),
    )));
    let rig = GitRig::via(
        vec![Hop::Proxy(ProxyAddr::new("127.0.0.1", squid.port()))],
        auth,
    )
    .await;
    parent.route(NAME, rig.host.addr());
    let dir = rig.work("w");
    let ran = rig
        .run(
            &dir,
            &["ls-remote", &format!("https://{NAME}/acme/web.git")],
        )
        .await;
    assert!(!ran.ok, "{}\n{}", ran.stdout, squid.access_log());
    assert_eq!(rig.host.seen().len(), 0, "{:#?}", rig.host.seen());
    assert!(
        parent.connects().is_empty(),
        "Squid never forwarded the tunnel"
    );
}

/// Clones, commits and pushes through whatever sits between the proxy and the host, and checks
/// that the host saw one credential on every request and the push.
async fn clone_and_push(rig: &GitRig, diagnostics: impl Fn() -> String) {
    let dir = rig.work("w");
    let url = format!("https://{NAME}/acme/web.git");
    let ran = rig.run(&dir, &["clone", "-q", &url, "web"]).await;
    assert!(ran.ok, "{}\n{}", ran.stderr, diagnostics());
    let clone = dir.join("web");
    std::fs::write(clone.join("lab.txt"), "through the lab\n").unwrap();
    for args in [&["add", "lab.txt"][..], &["commit", "-q", "-m", "lab"]] {
        let ran = rig.run(&clone, args).await;
        assert!(ran.ok, "{args:?}: {}", ran.stderr);
    }
    let ran = rig.run(&clone, &["push", "-q", "origin", "main"]).await;
    assert!(ran.ok, "{}\n{}", ran.stderr, diagnostics());
    let seen = rig.host.seen();
    assert!(seen.len() >= 4, "{seen:#?}");
    assert!(
        seen.iter()
            .all(|r| r.headers_named("authorization").len() == 1),
        "every request carries the injected credential: {seen:#?}"
    );
    assert!(
        seen.iter().any(|r| r.target.contains("git-receive-pack")),
        "the push reached the host"
    );
}

#[tokio::test]
async fn git_clones_and_pushes_through_a_proxy_that_demands_ntlm() {
    if !git_available() {
        return;
    }
    let host = GitHost::start(NAME, Versions::Http1).await;
    let Some(proxy) = NtlmProxy::start_on_ports(&[(NAME.to_owned(), Some(host.addr().port()))])
    else {
        return;
    };
    let auth = Arc::new(NegotiateAuth::new(Arc::new(HandBuiltNtlmSource {
        domain: "LAB",
        user: "alice",
    })));
    let rig = GitRig::via_host(
        host,
        vec![Hop::Proxy(ProxyAddr::new("127.0.0.1", proxy.port()))],
        auth,
        None,
    )
    .await;
    clone_and_push(&rig, || proxy.log()).await;
    assert!(
        matches!(proxy.legs().last(), Some(NtlmLeg::Accepted { user, ntlmv2: true, .. }) if user == "LAB\\alice"),
        "{:?}\n{}",
        proxy.legs(),
        proxy.log()
    );
}

#[tokio::test]
async fn git_clones_and_pushes_through_an_intercepting_proxy_whose_root_the_proxy_trusts() {
    if !git_available() {
        return;
    }
    let host = GitHost::start(NAME, Versions::Http1).await;
    let Some(mitm) = Interceptor::start_on_ports(&[(NAME.to_owned(), Some(host.addr().port()))])
    else {
        return;
    };
    let hops = vec![Hop::Proxy(ProxyAddr::new("127.0.0.1", mitm.port()))];
    let synced = vec![rustls::pki_types::CertificateDer::from(
        mitm.root().der().to_vec(),
    )];
    let rig = GitRig::via_host(host, hops, Arc::new(BasicAuth::new()), Some(synced)).await;
    clone_and_push(&rig, || mitm.log()).await;
    // The interceptor decrypted the traffic: its log has the push.
    assert!(mitm.log().contains("git-receive-pack"), "{}", mitm.log());
}

#[tokio::test]
async fn without_the_interceptors_root_the_clone_fails_and_no_credential_leaves() {
    if !git_available() {
        return;
    }
    let host = GitHost::start(NAME, Versions::Http1).await;
    let Some(mitm) = Interceptor::start_on_ports(&[(NAME.to_owned(), Some(host.addr().port()))])
    else {
        return;
    };
    let hops = vec![Hop::Proxy(ProxyAddr::new("127.0.0.1", mitm.port()))];
    // The proxy trusts only the host's own root: the interceptor's certificate chains elsewhere.
    let rig = GitRig::via_host(host, hops, Arc::new(BasicAuth::new()), None).await;
    let dir = rig.work("w");
    let ran = rig
        .run(
            &dir,
            &["ls-remote", &format!("https://{NAME}/acme/web.git")],
        )
        .await;
    assert!(!ran.ok, "{}\n{}", ran.stdout, mitm.log());
    assert!(ran.stderr.contains("502"), "{}", ran.stderr);
    assert_eq!(rig.host.seen().len(), 0, "{:#?}", rig.host.seen());
}
