// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote serving over real HTTP: the public names, the credential rule per `Host`, fetch
//! metadata, the opener policy, the registry entries that keep guests out, and the TLS listener
//! for a reverse proxy on another machine.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::SystemTime;

use puddle_api::{
    ApiConfig, ApiServer, ApiToken, EventHub, MemorySettings, NameResolver, NetworkEndpoint,
    NetworkListener, PublicHost, Remote, RemoteServing, RunningApi, Services, SettingsRepo,
    TlsIdentity, UiAssets, UiFile,
};
use puddle_netpolicy::{EndpointKind, OwnAddresses, PuddleEndpoints};
use puddle_store::{Limits, ManualClock, Store};
use rustls::pki_types::{CertificateDer, ServerName};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use crate::common::{START_MS, STEP, parse, raw};

#[derive(Debug)]
struct Files(HashMap<&'static str, (&'static str, &'static str)>);

impl UiAssets for Files {
    fn get(&self, path: &str) -> Option<UiFile> {
        self.0
            .get(path)
            .map(|(body, kind)| UiFile::new(body.as_bytes(), *kind))
    }
}

fn config(remote: Option<RemoteServing>) -> ApiConfig {
    let mut config = ApiConfig::default();
    config.ui = Some(Arc::new(Files(HashMap::from([(
        "index.html",
        ("<html><body>app shell</body></html>", "text/html"),
    )]))));
    config.extra_origins = vec!["tauri://localhost".to_owned()];
    config.remote = remote.map(Remote::new);
    config
}

fn serving(hosts: &[&str], base: Option<&str>) -> RemoteServing {
    let hosts = hosts
        .iter()
        .map(|h| PublicHost::parse(h).unwrap())
        .collect();
    RemoteServing::new(hosts, base).unwrap()
}

struct Bound {
    running: RunningApi,
    addr: SocketAddr,
    token: String,
    endpoints: PuddleEndpoints,
}

/// Answers from a fixed table; a name not in it does not resolve.
#[derive(Clone, Default)]
struct Table(Arc<std::sync::Mutex<HashMap<&'static str, Vec<IpAddr>>>>);

impl NameResolver for Table {
    fn resolve<'a>(
        &'a self,
        name: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send + 'a>> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .get(name)
                .cloned()
                .ok_or_else(|| std::io::Error::other("no such name"))
        })
    }
}

async fn bind(config: ApiConfig) -> Bound {
    bind_resolving(config, HashMap::new()).await
}

async fn bind_resolving(config: ApiConfig, records: HashMap<&'static str, Vec<IpAddr>>) -> Bound {
    bind_with(config, Table(Arc::new(std::sync::Mutex::new(records)))).await
}

async fn bind_with(config: ApiConfig, table: Table) -> Bound {
    let endpoints = PuddleEndpoints::new();
    let clock = Arc::new(ManualClock::new(START_MS));
    let store = Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap());
    let services = Services::new(
        store,
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        Arc::new(EventHub::default()),
        clock,
    )
    .with_endpoints(endpoints.clone())
    .with_front_resolver(Arc::new(table));
    let token = ApiToken::generate().unwrap();
    let server = ApiServer::bind(config, token.clone(), services)
        .await
        .unwrap();
    let addr = server.local_addr();
    Bound {
        running: server.spawn(),
        addr,
        token: token.expose().to_owned(),
        endpoints,
    }
}

fn request(path: &str, headers: &[(&str, &str)]) -> String {
    let mut text = format!("GET {path} HTTP/1.1\r\n");
    for (k, v) in headers {
        let _ = write!(text, "{k}: {v}\r\n");
    }
    text.push_str("Connection: close\r\n\r\n");
    text
}

impl Bound {
    async fn get(&self, path: &str, headers: &[(&str, &str)]) -> crate::common::Reply {
        raw(self.addr, request(path, headers).as_bytes()).await
    }

    fn loopback_host(&self) -> String {
        format!("127.0.0.1:{}", self.addr.port())
    }
}

#[tokio::test]
async fn with_remote_serving_off_a_public_name_is_refused_and_no_opener_policy_is_sent() {
    let api = bind(config(None)).await;
    let reply = api.get("/", &[("Host", "puddle.example.org")]).await;
    assert_eq!(reply.status, 421, "{reply:?}");
    let own = api.get("/", &[("Host", &api.loopback_host())]).await;
    assert_eq!(own.status, 200);
    assert_eq!(own.header("cross-origin-opener-policy"), None);
    assert!(
        api.endpoints
            .list()
            .iter()
            .all(|(_, k)| *k == EndpointKind::Api)
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn with_remote_serving_on_the_app_loads_under_a_public_name_with_the_opener_policy() {
    let api = bind(config(Some(serving(
        &["puddle.example.org", "alt.example.org:8443"],
        None,
    ))))
    .await;
    for host in [
        "puddle.example.org",
        "PUDDLE.example.org",
        "alt.example.org:8443",
    ] {
        let reply = api.get("/inbox", &[("Host", host)]).await;
        assert_eq!(reply.status, 200, "{host}: {reply:?}");
        assert_eq!(
            reply.header("cross-origin-opener-policy"),
            Some("same-origin")
        );
        assert_eq!(reply.header("x-frame-options"), Some("DENY"));
        assert!(
            reply
                .header("content-security-policy")
                .unwrap()
                .contains("frame-ancestors 'none'")
        );
    }
    let own = api.get("/", &[("Host", &api.loopback_host())]).await;
    assert_eq!(
        own.header("cross-origin-opener-policy"),
        Some("same-origin")
    );
    // Only the exact names: not the other's port, not a port left out, not a neighbour, not a
    // name below.
    for host in [
        "puddle.example.org:8443",
        "alt.example.org",
        "alt.example.org:443",
        "other.example.org",
        "ws.puddle.example.org",
        "puddle.example.org.evil.example",
        "puddle.example.org:80",
    ] {
        let reply = api.get("/", &[("Host", host)]).await;
        assert_eq!(reply.status, 421, "{host}: {reply:?}");
    }
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_install_token_is_accepted_for_a_loopback_host_only() {
    let api = bind(config(Some(serving(&["puddle.example.org"], None)))).await;
    let auth = format!("Bearer {}", api.token);
    let own = api
        .get(
            "/api/health",
            &[("Host", &api.loopback_host()), ("Authorization", &auth)],
        )
        .await;
    assert_eq!(own.status, 200, "{own:?}");
    let public = api
        .get(
            "/api/health",
            &[("Host", "puddle.example.org"), ("Authorization", &auth)],
        )
        .await;
    assert_eq!(public.status, 401, "{public:?}");
    assert_eq!(public.error(), "unauthorized");
    assert!(!public.body.contains(&api.token));
    let none = api
        .get("/api/health", &[("Host", "puddle.example.org")])
        .await;
    assert_eq!(none.status, 401);
    // A terminal's WebSocket path takes a ticket, not the token: still guarded by Host first.
    let wrong = api
        .get(
            "/api/health",
            &[("Host", "evil.example"), ("Authorization", &auth)],
        )
        .await;
    assert_eq!(wrong.status, 421);
    api.running.shutdown().await;
}

#[tokio::test]
async fn behind_a_public_name_an_origin_must_be_the_public_origin_and_same_origin() {
    let api = bind(config(Some(serving(&["puddle.example.org"], None)))).await;
    let host = ("Host", "puddle.example.org");
    let origin = ("Origin", "https://puddle.example.org");
    let api_ref = &api;
    let get = |extra: Vec<(&'static str, &'static str)>| {
        let mut headers = vec![host, origin];
        headers.extend(extra);
        async move { api_ref.get("/", &headers).await }
    };
    assert_eq!(
        get(vec![]).await.status,
        200,
        "a program sends no fetch metadata"
    );
    assert_eq!(
        get(vec![("Sec-Fetch-Site", "same-origin")]).await.status,
        200
    );
    for site in ["cross-site", "same-site", "none", "bogus"] {
        let reply = get(vec![("Sec-Fetch-Site", site)]).await;
        assert_eq!(reply.status, 403, "{site}: {reply:?}");
        assert_eq!(reply.error(), "forbidden_origin");
    }
    for origin in [
        "http://puddle.example.org",
        "https://evil.example",
        "https://ws.puddle.example.org",
        "null",
        "tauri://localhost",
    ] {
        let reply = api
            .get(
                "/",
                &[host, ("Origin", origin), ("Sec-Fetch-Site", "same-origin")],
            )
            .await;
        assert_eq!(reply.status, 403, "{origin}: {reply:?}");
    }
    // The loopback origin does not belong to a public name.
    let loopback_origin = format!("http://{}", api.loopback_host());
    let reply = api.get("/", &[host, ("Origin", &loopback_origin)]).await;
    assert_eq!(reply.status, 403);
    // And a request without an Origin is not asked for fetch metadata (a navigation).
    let nav = api
        .get("/", &[host, ("Sec-Fetch-Site", "cross-site")])
        .await;
    assert_eq!(nav.status, 200);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_desktop_origin_on_a_loopback_host_is_unchanged_by_remote_serving() {
    let api = bind(config(Some(serving(&["puddle.example.org"], None)))).await;
    let loopback = api.loopback_host();
    // The desktop webview's origin is cross-site to the loopback address and says so.
    for site in ["cross-site", "same-origin"] {
        let reply = api
            .get(
                "/",
                &[
                    ("Host", &loopback),
                    ("Origin", "tauri://localhost"),
                    ("Sec-Fetch-Site", site),
                ],
            )
            .await;
        assert_eq!(reply.status, 200, "{site}: {reply:?}");
    }
    // A public origin does not belong to a loopback host either: a front keeps the public `Host`.
    let reply = api
        .get(
            "/",
            &[
                ("Host", &loopback),
                ("Origin", "https://puddle.example.org"),
            ],
        )
        .await;
    assert_eq!(reply.status, 403, "{reply:?}");
    api.running.shutdown().await;
}

#[tokio::test]
async fn remote_serving_registers_the_front_and_blocks_the_names_until_it_stops() {
    let api = bind(config(Some(serving(
        &["puddle.example.org", "alt.example.org:8443"],
        Some("ws.example.net"),
    ))))
    .await;
    let own = OwnAddresses;
    for port in [443, 8443] {
        let dest = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port);
        assert_eq!(
            api.endpoints.find(dest, &own),
            Some(EndpointKind::ExternalFront),
            "{port}"
        );
    }
    assert_eq!(
        api.endpoints
            .find(SocketAddr::from(([8, 8, 8, 8], 443)), &own),
        None,
        "another host's port 443 is not ours to block"
    );
    for name in [
        "puddle.example.org",
        "ALT.example.org",
        "ws.example.net",
        "app.ws.example.net",
        "a.b.ws.example.net",
    ] {
        assert_eq!(
            api.endpoints.find_name(name),
            Some(EndpointKind::ExternalFront),
            "{name}"
        );
    }
    for name in [
        "example.net",
        "evilws.example.net",
        "example.org",
        "ws.example.net.evil.test",
    ] {
        assert_eq!(api.endpoints.find_name(name), None, "{name}");
    }
    let endpoints = api.endpoints.clone();
    api.running.shutdown().await;
    assert_eq!(endpoints.list(), []);
    assert_eq!(endpoints.find_name("puddle.example.org"), None);
}

#[tokio::test]
async fn a_front_on_another_machine_is_blocked_by_address_as_well_as_by_name() {
    let front: IpAddr = "192.168.1.50".parse().unwrap();
    let api = bind_resolving(
        config(Some(serving(
            &["puddle.example.org", "alt.example.org:8443"],
            Some("ws.example.net"),
        ))),
        HashMap::from([
            ("puddle.example.org", vec![front]),
            ("alt.example.org", vec![front]),
            ("ws.example.net", vec![front]),
        ]),
    )
    .await;
    let own = OwnAddresses;
    for port in [443, 8443] {
        assert_eq!(
            api.endpoints.find(SocketAddr::new(front, port), &own),
            Some(EndpointKind::ExternalFront),
            "{port}"
        );
    }
    // The mapped form of the same address.
    let mapped = SocketAddr::new("::ffff:192.168.1.50".parse().unwrap(), 443);
    assert_eq!(
        api.endpoints.find(mapped, &own),
        Some(EndpointKind::ExternalFront)
    );
    for dest in [
        SocketAddr::new(front, 22),
        SocketAddr::new("192.168.1.51".parse().unwrap(), 443),
    ] {
        assert_eq!(api.endpoints.find(dest, &own), None, "{dest}");
    }
    let endpoints = api.endpoints.clone();
    api.running.shutdown().await;
    assert_eq!(endpoints.find(SocketAddr::new(front, 443), &own), None);
}

#[tokio::test(start_paused = true)]
async fn a_front_whose_record_moves_is_followed_while_the_api_serves() {
    let old: IpAddr = "192.168.1.50".parse().unwrap();
    let new: IpAddr = "192.168.1.60".parse().unwrap();
    let table = Table::default();
    table
        .0
        .lock()
        .unwrap()
        .insert("puddle.example.org", vec![old]);
    let api = bind_with(
        config(Some(serving(&["puddle.example.org"], None))),
        table.clone(),
    )
    .await;
    let own = OwnAddresses;
    let blocked = |ip| api.endpoints.find(SocketAddr::new(ip, 443), &own).is_some();
    assert!(blocked(old));
    assert!(!blocked(new));
    table
        .0
        .lock()
        .unwrap()
        .insert("puddle.example.org", vec![new]);
    tokio::time::sleep(std::time::Duration::from_secs(70)).await;
    assert!(
        blocked(new),
        "the new address is blocked after the next lookup"
    );
    assert!(!blocked(old), "the old one is let go");
    api.running.shutdown().await;
}

// ---- the network listener ----

struct Proxy {
    api: Bound,
    addr: SocketAddr,
    cert_der: Vec<u8>,
    _dir: tempfile::TempDir,
}

/// A remote API with a TLS listener on loopback that admits `allowed`.
async fn proxied(allowed: &[IpAddr]) -> Proxy {
    let dir = tempfile::tempdir().unwrap();
    let cert = puddle_certs::devcert::listener::ensure(
        dir.path(),
        &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        &["puddle.example.org".to_owned()],
        SystemTime::now(),
    )
    .unwrap();
    let mut config = config(Some(serving(&["puddle.example.org"], None)));
    config.remote = config.remote.map(|remote| {
        remote.with_network_listener(NetworkListener::new(
            NetworkEndpoint::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0, allowed.to_vec()).unwrap(),
            TlsIdentity::new(cert.cert_der().to_vec(), cert.key().pkcs8_der().to_vec()),
        ))
    });
    let api = bind(config).await;
    let addr = api.running.network_addr().expect("a network listener");
    Proxy {
        api,
        addr,
        cert_der: cert.cert_der().to_vec(),
        _dir: dir,
    }
}

fn connector(trust: &[u8]) -> TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(trust.to_vec())).unwrap();
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    TlsConnector::from(Arc::new(config))
}

impl Proxy {
    /// One request over TLS, the way a proxy would send it.
    async fn tls_get(&self, path: &str, headers: &[(&str, &str)]) -> crate::common::Reply {
        let tcp = TcpStream::connect(self.addr).await.unwrap();
        let name = ServerName::try_from("puddle.example.org").unwrap();
        let mut stream = tokio::time::timeout(STEP, connector(&self.cert_der).connect(name, tcp))
            .await
            .unwrap()
            .expect("the TLS handshake");
        stream
            .write_all(request(path, headers).as_bytes())
            .await
            .unwrap();
        let mut bytes = Vec::new();
        // The server closes without a close_notify on `Connection: close`; what arrived counts.
        let _ = tokio::time::timeout(STEP, stream.read_to_end(&mut bytes))
            .await
            .unwrap();
        parse(&bytes)
    }
}

#[tokio::test]
async fn an_allowed_proxy_reaches_the_api_over_tls_under_a_public_name() {
    let proxy = proxied(&[IpAddr::V4(Ipv4Addr::LOCALHOST)]).await;
    let app = proxy
        .tls_get(
            "/",
            &[
                ("Host", "puddle.example.org"),
                ("Origin", "https://puddle.example.org"),
            ],
        )
        .await;
    assert_eq!(app.status, 200, "{app:?}");
    assert_eq!(
        app.header("cross-origin-opener-policy"),
        Some("same-origin")
    );
    // The install token is no credential here either.
    let auth = format!("Bearer {}", proxy.api.token);
    let api = proxy
        .tls_get(
            "/api/health",
            &[("Host", "puddle.example.org"), ("Authorization", &auth)],
        )
        .await;
    assert_eq!(api.status, 401, "{api:?}");
    proxy.api.running.shutdown().await;
}

#[tokio::test]
async fn the_network_listener_refuses_every_loopback_host() {
    let proxy = proxied(&[IpAddr::V4(Ipv4Addr::LOCALHOST)]).await;
    let auth = format!("Bearer {}", proxy.api.token);
    for host in [
        format!("127.0.0.1:{}", proxy.api.addr.port()),
        format!("localhost:{}", proxy.api.addr.port()),
        format!("127.0.0.1:{}", proxy.addr.port()),
        "localhost".to_owned(),
        "evil.example".to_owned(),
    ] {
        let reply = proxy
            .tls_get("/api/health", &[("Host", &host), ("Authorization", &auth)])
            .await;
        assert_eq!(reply.status, 421, "{host}: {reply:?}");
    }
    // The same host on the loopback listener still works with the token.
    let own = proxy
        .api
        .get(
            "/api/health",
            &[
                ("Host", &proxy.api.loopback_host()),
                ("Authorization", &auth),
            ],
        )
        .await;
    assert_eq!(own.status, 200);
    proxy.api.running.shutdown().await;
}

#[tokio::test]
async fn a_peer_that_is_not_listed_is_closed_at_accept_before_any_handshake() {
    let proxy = proxied(&[IpAddr::V4(Ipv4Addr::new(192, 0, 2, 77))]).await;
    // The connection is made by the kernel, then closed by the server without a byte.
    let mut tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let mut buf = [0_u8; 16];
    let read = tokio::time::timeout(STEP, tcp.read(&mut buf))
        .await
        .unwrap();
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
    // A client that tries the handshake fails, and never sees a certificate.
    let name = ServerName::try_from("puddle.example.org").unwrap();
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let handshake = tokio::time::timeout(STEP, connector(&proxy.cert_der).connect(name, tcp))
        .await
        .unwrap();
    assert!(handshake.is_err());
    proxy.api.running.shutdown().await;
}

#[tokio::test]
async fn the_network_listener_speaks_tls_only_and_trusts_only_its_own_certificate() {
    let proxy = proxied(&[IpAddr::V4(Ipv4Addr::LOCALHOST)]).await;
    // Plain HTTP gets no answer.
    let mut tcp = TcpStream::connect(proxy.addr).await.unwrap();
    tcp.write_all(request("/", &[("Host", "puddle.example.org")]).as_bytes())
        .await
        .unwrap();
    let mut bytes = Vec::new();
    let _ = tokio::time::timeout(STEP, tcp.read_to_end(&mut bytes))
        .await
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&bytes).contains("HTTP/1.1 200"),
        "plain HTTP was answered: {bytes:?}"
    );
    // A client that does not trust the certificate refuses it (a proxy must pin or trust it).
    let other = puddle_certs::devcert::listener::ensure(
        tempfile::tempdir().unwrap().path(),
        &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        &["puddle.example.org".to_owned()],
        SystemTime::now(),
    )
    .unwrap();
    let tcp = TcpStream::connect(proxy.addr).await.unwrap();
    let name = ServerName::try_from("puddle.example.org").unwrap();
    let result = connector(other.cert_der()).connect(name, tcp).await;
    assert!(result.is_err(), "a certificate nobody trusted was accepted");
    // The listener's address is in the registry for guests, and leaves it with the API.
    let endpoints = proxy.api.endpoints.clone();
    assert!(
        endpoints
            .list()
            .contains(&(proxy.addr, EndpointKind::RemoteApi))
    );
    assert_eq!(
        endpoints.find(proxy.addr, &OwnAddresses),
        Some(EndpointKind::RemoteApi)
    );
    proxy.api.running.shutdown().await;
    assert_eq!(endpoints.find(proxy.addr, &OwnAddresses), None);
}

#[tokio::test]
async fn a_network_listener_with_an_unusable_certificate_does_not_bind() {
    let clock = Arc::new(ManualClock::new(START_MS));
    let services = Services::new(
        Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap()),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        Arc::new(EventHub::default()),
        clock,
    );
    let mut broken = config(Some(serving(&["puddle.example.org"], None)));
    broken.remote = broken.remote.map(|remote| {
        remote.with_network_listener(NetworkListener::new(
            NetworkEndpoint::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                0,
                vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            )
            .unwrap(),
            TlsIdentity::new(vec![1, 2, 3], vec![4, 5, 6]),
        ))
    });
    let err = ApiServer::bind(broken, ApiToken::generate().unwrap(), services)
        .await
        .err()
        .unwrap();
    assert!(err.to_string().contains("TLS certificate"), "{err}");
}

#[tokio::test]
async fn a_network_listener_on_a_taken_port_says_so() {
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let dir = tempfile::tempdir().unwrap();
    let cert = puddle_certs::devcert::listener::ensure(
        dir.path(),
        &[IpAddr::V4(Ipv4Addr::LOCALHOST)],
        &[],
        SystemTime::now(),
    )
    .unwrap();
    let mut config = config(Some(serving(&["puddle.example.org"], None)));
    config.remote = config.remote.map(|remote| {
        remote.with_network_listener(NetworkListener::new(
            NetworkEndpoint::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                port,
                vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            )
            .unwrap(),
            TlsIdentity::new(cert.cert_der().to_vec(), cert.key().pkcs8_der().to_vec()),
        ))
    });
    let clock = Arc::new(ManualClock::new(START_MS));
    let services = Services::new(
        Arc::new(Store::open_in_memory(clock.clone(), Limits::default()).unwrap()),
        Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>,
        Arc::new(EventHub::default()),
        clock,
    );
    let err = ApiServer::bind(config, ApiToken::generate().unwrap(), services)
        .await
        .err()
        .unwrap();
    assert!(
        err.to_string().contains(&format!(
            "cannot listen on 127.0.0.1:{port} for a reverse proxy"
        )),
        "{err}"
    );
}
