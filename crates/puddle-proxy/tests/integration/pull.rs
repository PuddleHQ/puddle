// SPDX-License-Identifier: AGPL-3.0-or-later
//! I tests of the image-pull proxy: a real loopback listener, raw HTTP clients and local
//! servers. The bars: only `127.0.0.1`; a missing or wrong token is a `407` and nothing is
//! connected; the right token tunnels and forwards; the guard blocks Puddle's own endpoints and
//! metadata; the token never shows up in a log line
//! (`pull_logs.rs`, its own process for a global subscriber).
use crate::pull_support;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::future::BoxFuture;
use puddle_netpolicy::{EndpointKind, LocalAccess, PuddleEndpoints};
use puddle_proxy::testing::StaticResolver;
use puddle_proxy::{ProxyConfig, PullProxy, Resolver};
use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionOrigin, ConnectionOutcome, ConnectionReason,
    DomainName, LocalCategory,
};
use pull_support::{LOCAL, connect, echo, http_server, pull_proxy, pull_proxy_logged, send};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn the_listener_is_loopback_only_and_registered_while_it_serves() {
    let endpoints = PuddleEndpoints::new();
    let proxy = PullProxy::bind(&endpoints).unwrap();
    let addr = proxy.local_addr();
    assert_eq!(addr.ip(), LOCAL);
    assert_eq!(proxy.proxy_url().addr(), addr);
    assert_eq!(endpoints.list(), [(addr, EndpointKind::PullProxy)]);
    let url = proxy.proxy_url();
    assert_eq!(
        url.expose(),
        format!("http://puddle:{}@{addr}", proxy.token().expose())
    );
    let route = proxy.serve().unwrap();
    assert_eq!(route.local_addr(), addr);
    assert_eq!(endpoints.list().len(), 1);
    route.shutdown().await;
    assert!(endpoints.list().is_empty(), "unregistered on shutdown");
    // Each run has its own token.
    let a = PullProxy::bind(&endpoints).unwrap();
    let b = PullProxy::bind(&endpoints).unwrap();
    assert_ne!(a.token().expose(), b.token().expose());
}

/// `shutdown` returns only once the listener is gone and open tunnels are reset: the caller
/// may rebind the address or tear down what the tunnels reach straight away. The checks run
/// with no await in between, so a shutdown that left the cancelling to the runtime fails them.
#[tokio::test]
async fn a_shut_down_pull_proxy_has_closed_its_listener_and_its_tunnels_on_return() {
    let endpoints = PuddleEndpoints::new();
    let server = echo().await;
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let addr = route.local_addr();
    let target = format!("registry.test:{}", server.addr.port());
    let (head, mut tunnel) = send(addr, &connect(&target, Some(&format!("Basic {good}")))).await;
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    tunnel.write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    tunnel.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");

    route.shutdown().await;

    let rebound = std::net::TcpListener::bind(addr);
    assert!(
        rebound.is_ok(),
        "the address must be free again when shutdown returns: {rebound:?}"
    );
    let ended = tokio::time::timeout(Duration::from_secs(5), tunnel.read(&mut back))
        .await
        .expect("an open tunnel must be reset by shutdown, not left hanging");
    assert!(matches!(ended, Ok(0) | Err(_)), "{ended:?}");
}

#[tokio::test]
async fn a_missing_or_wrong_token_is_a_407_and_nothing_is_connected() {
    let endpoints = PuddleEndpoints::new();
    let server = echo().await;
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let target = format!("registry.test:{}", server.addr.port());
    let wrong = BASE64.encode(format!("puddle:{}", "0".repeat(64)));
    let other_user = BASE64.encode("someone:else");
    for auth in [
        None,
        Some(format!("Basic {wrong}")),
        Some(format!("Basic {other_user}")),
        Some(format!("Bearer {good}")),
        Some("Basic".to_owned()),
    ] {
        let (head, mut s) = send(route.local_addr(), &connect(&target, auth.as_deref())).await;
        assert!(
            head.starts_with("HTTP/1.1 407 "),
            "{auth:?} must be refused: {head}"
        );
        assert!(
            head.contains("proxy-authenticate: Basic realm=\"puddle\""),
            "{head}"
        );
        let mut body = String::new();
        s.read_to_string(&mut body).await.unwrap();
        assert!(body.contains("only Puddle may use it"), "{body}");
    }
    // A plain-HTTP request without credentials is refused the same way.
    let (head, _) = send(
        route.local_addr(),
        &format!("GET http://{target}/ HTTP/1.1\r\nHost: {target}\r\n\r\n"),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 407 "), "{head}");
    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        0,
        "nothing reached the server"
    );
}

#[tokio::test]
async fn the_right_token_tunnels_both_ways() {
    let endpoints = PuddleEndpoints::new();
    let server = echo().await;
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let target = format!("registry.test:{}", server.addr.port());
    let (head, mut s) = send(
        route.local_addr(),
        &connect(&target, Some(&format!("Basic {good}"))),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    let payload = vec![7u8; 256 * 1024];
    let (mut r, mut w) = s.split();
    let ((), echoed) = tokio::join!(
        async {
            w.write_all(&payload).await.unwrap();
            w.shutdown().await.unwrap();
        },
        async {
            let mut back = Vec::new();
            r.read_to_end(&mut back).await.unwrap();
            back
        }
    );
    assert_eq!(echoed, payload);
    assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
}

/// The pull proxy carries registry traffic only, so it never looks for SSH: a first upload that
/// starts like an SSH identification line is passed on like any other bytes.
#[tokio::test]
async fn a_pull_tunnel_passes_on_bytes_that_look_like_an_ssh_banner() {
    let endpoints = PuddleEndpoints::new();
    let server = echo().await;
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let target = format!("registry.test:{}", server.addr.port());
    let (head, mut s) = send(
        route.local_addr(),
        &connect(&target, Some(&format!("Basic {good}"))),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    let banner = b"SSH-2.0-OpenSSH_10.0\r\n";
    s.write_all(banner).await.unwrap();
    s.shutdown().await.unwrap();
    let mut back = Vec::new();
    s.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, banner);
}

#[tokio::test]
async fn plain_http_is_forwarded_without_the_proxy_credentials() {
    let endpoints = PuddleEndpoints::new();
    let (server, heads) = http_server().await;
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let target = format!("registry.test:{}", server.port());
    let (head, mut s) = send(
        route.local_addr(),
        &format!(
            "GET http://{target}/v2/ HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: Basic {good}\r\nAccept: */*\r\n\r\n"
        ),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    let mut body = String::new();
    s.read_to_string(&mut body).await.unwrap();
    assert_eq!(body, "hello");
    let heads = heads.lock().unwrap().clone();
    assert_eq!(heads.len(), 1);
    let upstream = &heads[0];
    assert!(upstream.starts_with("GET /v2/ HTTP/1.1\r\n"), "{upstream}");
    assert!(
        !upstream
            .to_ascii_lowercase()
            .contains("proxy-authorization"),
        "{upstream}"
    );
    assert!(!upstream.contains(&good), "{upstream}");
}

#[tokio::test]
async fn puddles_own_endpoints_and_metadata_are_blocked() {
    let endpoints = PuddleEndpoints::new();
    let api = echo().await;
    let _api = endpoints.register(api.addr, EndpointKind::Api);
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let auth = format!("Basic {good}");
    let own = route.local_addr();
    for (target, code) in [
        (format!("127.0.0.1:{}", own.port()), "puddle_endpoint"),
        (format!("localhost:{}", own.port()), "puddle_endpoint"),
        (
            format!("registry.test:{}", api.addr.port()),
            "puddle_endpoint",
        ),
        (format!("127.0.0.1:{}", api.addr.port()), "puddle_endpoint"),
        ("169.254.169.254:80".to_owned(), "toggle:metadata"),
        ("meta.test:80".to_owned(), "toggle:metadata"),
        ("metadata.google.internal:80".to_owned(), "toggle:metadata"),
        ("169.254.10.10:80".to_owned(), "toggle:link_local"),
    ] {
        let (head, _) = send(own, &connect(&target, Some(&auth))).await;
        assert!(head.starts_with("HTTP/1.1 403 "), "{target}: {head}");
        assert!(
            head.contains(&format!("x-puddle-blocked: {code}")),
            "{target}: {head}"
        );
    }
    assert_eq!(api.accepted.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn loopback_can_be_turned_off() {
    let endpoints = PuddleEndpoints::new();
    let server = echo().await;
    let proxy = PullProxy::bind(&endpoints)
        .unwrap()
        .with_resolver(Arc::new(
            StaticResolver::new().with("registry.test", &[LOCAL]),
        ))
        .with_local_access(LocalAccess::NONE);
    let good = BASE64.encode(format!("puddle:{}", proxy.token().expose()));
    let route = proxy.serve().unwrap();
    let target = format!("registry.test:{}", server.addr.port());
    let (head, _) = send(
        route.local_addr(),
        &connect(&target, Some(&format!("Basic {good}"))),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    assert!(head.contains("x-puddle-blocked: toggle:loopback"), "{head}");
    let (head, _) = send(
        route.local_addr(),
        &connect("localhost:1", Some(&format!("Basic {good}"))),
    )
    .await;
    assert!(head.contains("x-puddle-blocked: toggle:loopback"), "{head}");
}

#[tokio::test]
async fn unresolvable_and_unreachable_registries_are_502() {
    let endpoints = PuddleEndpoints::new();
    let (route, good) = pull_proxy(&endpoints, None);
    let auth = format!("Basic {good}");
    let (head, _) = send(
        route.local_addr(),
        &connect("registry.test:443", Some(&auth)),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 502 "), "{head}");
    // A loopback port nothing listens on.
    let closed = puddle_test_kit::ReservedPort::new();
    let port = closed.port();
    let (head, _) = send(
        route.local_addr(),
        &connect(&format!("127.0.0.1:{port}"), Some(&auth)),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 502 "), "{head}");
    let (head, _) = send(route.local_addr(), &connect("not a host", Some(&auth))).await;
    assert!(head.starts_with("HTTP/1.1 400 "), "{head}");
}

#[tokio::test]
async fn a_tunnel_is_recorded_as_puddles_own_connection_with_its_bytes() {
    let endpoints = PuddleEndpoints::new();
    let server = echo().await;
    let (route, good, log) = pull_proxy_logged(&endpoints, Some(LOCAL));
    let target = format!("registry.test:{}", server.addr.port());
    let (head, mut s) = send(
        route.local_addr(),
        &connect(&target, Some(&format!("Basic {good}"))),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    s.write_all(b"layer bytes").await.unwrap();
    let mut back = [0u8; 11];
    s.read_exact(&mut back).await.unwrap();
    drop(s);
    let events = log.wait_for(1, Duration::from_secs(5)).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let event = &events[0];
    assert_eq!(event.origin, ConnectionOrigin::Puddle);
    assert_eq!(event.workspace, None);
    assert_eq!(
        (event.host.to_string(), event.port),
        ("registry.test".to_owned(), server.addr.port())
    );
    assert_eq!(
        (event.decision, event.reason),
        (ConnectionDecision::Allow, ConnectionReason::PuddleRequest)
    );
    assert_eq!(event.resolved_ip, Some(LOCAL));
    assert_eq!(event.upstream, None);
    assert_eq!((event.rule_id, event.pending_id), (None, None));
    // Counted on the client side, the proxy's own 200 included.
    assert!(event.bytes_up >= 11 && event.bytes_down >= 11, "{event:?}");
}

#[tokio::test]
async fn plain_http_pulls_record_the_method_and_path_without_the_query() {
    let endpoints = PuddleEndpoints::new();
    let (server, _heads) = http_server().await;
    let (route, good, log) = pull_proxy_logged(&endpoints, Some(LOCAL));
    let target = format!("registry.test:{}", server.port());
    let (_, mut s) = send(
        route.local_addr(),
        &format!(
            "GET http://{target}/v2/?token=secret HTTP/1.1\r\nHost: {target}\r\nProxy-Authorization: Basic {good}\r\n\r\n"
        ),
    )
    .await;
    let mut body = String::new();
    s.read_to_string(&mut body).await.unwrap();
    let events = log.wait_for(1, Duration::from_secs(5)).await;
    let http = events[0].http.as_ref().unwrap();
    assert_eq!((http.method(), http.path()), ("GET", "/v2/"));
    assert!(!format!("{events:?}").contains("secret"));
}

#[tokio::test]
async fn refused_pulls_are_recorded_and_unauthorised_ones_are_not() {
    let endpoints = PuddleEndpoints::new();
    let (route, good, log) = pull_proxy_logged(&endpoints, None);
    let own = route.local_addr();
    // No credentials: the destination is never read, so nothing is recorded.
    let (head, _) = send(own, &connect("registry.test:443", None)).await;
    assert!(head.starts_with("HTTP/1.1 407 "), "{head}");
    let auth = format!("Basic {good}");
    let (head, _) = send(own, &connect("169.254.169.254:80", Some(&auth))).await;
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    let (head, mut stream) = send(own, &connect("registry.test:443", Some(&auth))).await;
    assert!(head.starts_with("HTTP/1.1 502 "), "{head}");
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(body.contains("no such name"), "{body}");
    let events = log.wait_for(2, Duration::from_secs(5)).await;
    assert_eq!(events.len(), 2, "{events:?}");
    assert!(events.iter().all(|e| e.origin == ConnectionOrigin::Puddle));
    let blocked = events
        .iter()
        .find(|e| e.decision == ConnectionDecision::Blocked)
        .unwrap();
    assert_eq!(
        blocked.reason,
        ConnectionReason::Blocked(BlockReason::LocalToggle(LocalCategory::Metadata))
    );
    assert_eq!(blocked.resolved_ip, None);
    // A name that doesn't resolve: Puddle allowed it, nothing connected, and the record says so.
    let failed = events
        .iter()
        .find(|e| e.decision == ConnectionDecision::Allow)
        .unwrap();
    assert_eq!(failed.resolved_ip, None);
    assert_eq!(failed.host.to_string(), "registry.test");
    assert_eq!(
        failed.reason,
        ConnectionReason::PuddleRequestFailed("unreachable"),
        "a pull that got nowhere must not read as one that went through"
    );
}

/// A resolver whose lookups never finish.
struct Stuck;

impl Resolver for Stuck {
    fn resolve<'a>(
        &'a self,
        _name: &'a DomainName,
        _port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<std::net::SocketAddr>>> {
        Box::pin(std::future::pending())
    }
}

/// A resolver whose lookups fail without saying anything about the name.
struct Broken;

impl Resolver for Broken {
    fn resolve<'a>(
        &'a self,
        _name: &'a DomainName,
        _port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<std::net::SocketAddr>>> {
        Box::pin(async { Err(std::io::Error::other("no resolver answered")) })
    }
}

#[tokio::test]
async fn a_pull_says_when_the_lookup_failed_rather_than_the_name_not_existing() {
    let endpoints = PuddleEndpoints::new();
    let proxy = PullProxy::bind(&endpoints)
        .unwrap()
        .with_resolver(Arc::new(Broken));
    let auth = format!(
        "Basic {}",
        BASE64.encode(format!("puddle:{}", proxy.token().expose()))
    );
    let route = proxy.serve().unwrap();
    let (head, mut stream) = send(
        route.local_addr(),
        &connect("registry.test:443", Some(&auth)),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 502 "), "{head}");
    let mut body = String::new();
    stream.read_to_string(&mut body).await.unwrap();
    assert!(body.contains("lookup for registry.test failed"), "{body}");
    assert!(!body.contains("no such name"), "{body}");
}

#[tokio::test]
async fn a_pull_that_could_not_resolve_its_name_records_why_it_failed() {
    let endpoints = PuddleEndpoints::new();
    let log = Arc::new(puddle_proxy::testing::CollectingConnectionLog::new());
    let mut config = ProxyConfig::default();
    config.resolve_timeout = Duration::from_millis(100);
    let proxy = PullProxy::bind(&endpoints)
        .unwrap()
        .with_resolver(Arc::new(Stuck))
        .with_config(config)
        .with_connection_log(log.clone());
    let auth = format!(
        "Basic {}",
        BASE64.encode(format!("puddle:{}", proxy.token().expose()))
    );
    let route = proxy.serve().unwrap();
    // The lookup never finishes: a 504 and a timeout in the record.
    let (head, _) = send(route.local_addr(), &connect("slow.test:443", Some(&auth))).await;
    assert!(head.starts_with("HTTP/1.1 504 "), "{head}");
    let events = log.wait_for(1, Duration::from_secs(5)).await;
    assert_eq!(events[0].outcome, Some(ConnectionOutcome::Timeout));
    assert_eq!(
        events[0].reason,
        ConnectionReason::PuddleRequestFailed("timeout")
    );
}

#[tokio::test]
async fn a_pull_whose_name_has_no_address_or_does_not_resolve_is_a_connect_failure_in_the_record() {
    let endpoints = PuddleEndpoints::new();
    let log = Arc::new(puddle_proxy::testing::CollectingConnectionLog::new());
    let proxy = PullProxy::bind(&endpoints)
        .unwrap()
        .with_resolver(Arc::new(StaticResolver::new().with("empty.test", &[])))
        .with_connection_log(log.clone());
    let auth = format!(
        "Basic {}",
        BASE64.encode(format!("puddle:{}", proxy.token().expose()))
    );
    let route = proxy.serve().unwrap();
    for name in ["empty.test", "unknown.test"] {
        let (head, _) = send(
            route.local_addr(),
            &connect(&format!("{name}:443"), Some(&auth)),
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 502 "), "{name}: {head}");
    }
    let events = log.wait_for(2, Duration::from_secs(5)).await;
    assert!(
        events
            .iter()
            .all(|e| e.outcome == Some(ConnectionOutcome::ConnectFailed)),
        "{events:?}"
    );
}

#[tokio::test]
async fn localhost_on_another_port_is_tunnelled_while_loopback_is_on() {
    let endpoints = PuddleEndpoints::new();
    let server = echo().await;
    let proxy = PullProxy::bind(&endpoints)
        .unwrap()
        .with_resolver(Arc::new(StaticResolver::new().with("localhost", &[LOCAL])));
    let good = BASE64.encode(format!("puddle:{}", proxy.token().expose()));
    let route = proxy.serve().unwrap();
    let target = format!("localhost:{}", server.addr.port());
    let (head, _) = send(
        route.local_addr(),
        &connect(&target, Some(&format!("Basic {good}"))),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    assert_eq!(server.accepted.load(Ordering::SeqCst), 1);
}

/// A connection that is open when the route stops: once the proxy's task is gone the socket is
/// closed, so a read ends with end of file or a reset instead of waiting.
async fn open_connection(route: &puddle_proxy::PullRoute, good: &str) -> tokio::net::TcpStream {
    let server = echo().await;
    let target = format!("registry.test:{}", server.addr.port());
    let (head, s) = send(
        route.local_addr(),
        &connect(&target, Some(&format!("Basic {good}"))),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 200 "), "{head}");
    s
}

#[tokio::test]
async fn shutdown_returns_only_when_the_listener_and_its_connections_are_gone() {
    let endpoints = PuddleEndpoints::new();
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let addr = route.local_addr();
    let open = open_connection(&route, &good).await;
    route.shutdown().await;
    // Nothing below yields to the runtime, so a task that is only marked for abort is still alive.
    let mut buf = [0u8; 1];
    assert!(
        matches!(open.try_read(&mut buf), Ok(0) | Err(_)),
        "the open connection is closed"
    );
    // Not "the port refuses connections": the proxy bound port 0, and another test's socket can hold
    // that number by now. The proxy answers a request with a wrong token 407; a stranger does not.
    assert_ne!(
        puddle_test_kit::http_status(addr, "/", "wrong"),
        Some(407),
        "the listener is closed"
    );
}

#[tokio::test]
async fn dropping_the_route_stops_the_proxy_and_closes_its_connections() {
    let endpoints = PuddleEndpoints::new();
    let (route, good) = pull_proxy(&endpoints, Some(LOCAL));
    let mut open = open_connection(&route, &good).await;
    drop(route);
    let mut buf = [0u8; 1];
    // The bound only keeps a failing run from hanging: the read ends when the task is aborted.
    let read = tokio::time::timeout(Duration::from_secs(30), open.read(&mut buf))
        .await
        .expect("a dropped route closes its connections");
    assert!(matches!(read, Ok(0) | Err(_)));
}

#[tokio::test]
async fn a_pull_to_a_name_puddle_serves_under_is_refused_before_any_lookup() {
    let endpoints = PuddleEndpoints::new();
    let _name = endpoints.block_name("front.puddle.example", true, EndpointKind::Api);
    // The resolver would answer with a public address: only the name stage can refuse it.
    let (route, good) = pull_proxy(&endpoints, Some(std::net::Ipv4Addr::new(8, 8, 8, 8).into()));
    let auth = format!("Basic {good}");
    for target in ["front.puddle.example:443", "ws.front.puddle.example:443"] {
        let (head, _) = send(route.local_addr(), &connect(target, Some(&auth))).await;
        assert!(head.starts_with("HTTP/1.1 403 "), "{target}: {head}");
        assert!(
            head.contains("x-puddle-blocked: puddle_endpoint"),
            "{target}: {head}"
        );
    }
}

#[tokio::test]
async fn a_name_under_a_company_dns_suffix_answers_to_the_company_toggle_even_on_a_private_address()
{
    let signals = Arc::new(puddle_netpolicy::StaticSignals::new().with_dns_suffix("corp.example"));
    let resolver = || {
        Arc::new(
            StaticResolver::new().with("registry.corp.example", &["10.1.2.3".parse().unwrap()]),
        )
    };
    // Private networks on, company network off: the name makes the address the company's.
    let off = LocalAccess::NONE.with_toggle(LocalCategory::Private, true);
    let proxy = PullProxy::bind(&PuddleEndpoints::new())
        .unwrap()
        .with_resolver(resolver())
        .with_company_signals(signals)
        .with_local_access(off);
    let auth = format!(
        "Basic {}",
        BASE64.encode(format!("puddle:{}", proxy.token().expose()))
    );
    let route = proxy.serve().unwrap();
    let (head, _) = send(
        route.local_addr(),
        &connect("registry.corp.example:443", Some(&auth)),
    )
    .await;
    assert!(head.starts_with("HTTP/1.1 403 "), "{head}");
    assert!(
        head.contains("x-puddle-blocked: toggle:company_network"),
        "{head}"
    );
}
