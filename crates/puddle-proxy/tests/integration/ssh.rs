// SPDX-License-Identifier: GPL-3.0-or-later
//! I tests for SSH from a workspace. SSH to a Git host's name is refused at once, whatever the
//! port, with the reason `ssh_unsupported`; it never reaches the rules, the inbox or the
//! resolver, and nothing of it reaches a server. SSH to any other name is decided by the rules like
//! every connection and spliced when allowed. A fake guest (yamux client) on a real per-workspace
//! endpoint, the real proxy behind it.
#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use futures_util::StreamExt;
use puddle_agent_proto::tokio_yamux::{Control, Session, StreamHandle};
use puddle_agent_proto::yamux::client_config;
use puddle_ipc::IpcRoot;
use puddle_proxy::testing::{AnyAddress, CollectingConnectionLog, StaticPolicy, StaticResolver};
use puddle_proxy::{BoxFuture, PortProbe, Proxy, Resolver, Route};
use puddle_types::{
    BlockReason, ConnectionDecision, ConnectionEvent, ConnectionReason, Host, NullSink, WorkspaceId,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const BANNER: &[u8] = b"SSH-2.0-OpenSSH_10.0p2 Debian-7\r\n";

fn host(h: &str) -> Host {
    Host::parse_normalised(h).unwrap()
}

struct Rig {
    policy: Arc<StaticPolicy>,
    resolver: Arc<StaticResolver>,
    log: Arc<CollectingConnectionLog>,
    route: Route,
    _root: IpcRoot,
}

impl Rig {
    /// Loopback servers are reachable (`AnyAddress`), and `names` resolve to loopback.
    fn new(names: &[&str]) -> Self {
        Self::build(names, true)
    }

    /// The default address check: loopback is blocked until its toggle is on.
    fn with_default_guard() -> Self {
        Self::build(&[], false)
    }

    fn build(names: &[&str], any_address: bool) -> Self {
        Self::build_with(names, any_address, |proxy| proxy)
    }

    /// As [`Self::new`], with `tune` applied to the proxy before it serves.
    fn with_proxy(names: &[&str], tune: impl FnOnce(Proxy) -> Proxy) -> Self {
        Self::build_with(names, true, tune)
    }

    fn build_with(names: &[&str], any_address: bool, tune: impl FnOnce(Proxy) -> Proxy) -> Self {
        let policy = Arc::new(StaticPolicy::new());
        let log = Arc::new(CollectingConnectionLog::new());
        let resolver = names
            .iter()
            .fold(StaticResolver::new(), |r, n| r.with(n, &[LOCAL]));
        let resolver = Arc::new(resolver);
        let mut proxy = Proxy::new(policy.clone(), Arc::new(NullSink))
            .with_connection_log(log.clone())
            .with_resolver(resolver.clone());
        if any_address {
            proxy = proxy.with_address_check(Arc::new(AnyAddress));
        }
        let proxy = Arc::new(tune(proxy));
        let root = IpcRoot::new().unwrap();
        let route = proxy.serve_route(root.listen().unwrap(), WorkspaceId::new("box").unwrap());
        Self {
            policy,
            resolver,
            log,
            route,
            _root: root,
        }
    }

    async fn guest(&self) -> Guest {
        let conn = puddle_ipc::connect(self.route.endpoint().path())
            .await
            .unwrap();
        let mut session = Session::new_client(conn, client_config());
        let control = session.control();
        let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
        Guest { control, driver }
    }

    async fn events(&self, count: usize) -> Vec<ConnectionEvent> {
        let events = self.log.wait_for(count, Duration::from_secs(5)).await;
        assert!(events.len() >= count, "{count} events expected: {events:?}");
        events
    }
}

struct Guest {
    control: Control,
    driver: JoinHandle<()>,
}

impl Drop for Guest {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

impl Guest {
    /// Sends `bytes` on a new stream and reads the answer to its end.
    async fn exchange(&mut self, bytes: &[u8]) -> String {
        let mut stream = self.control.open_stream().await.unwrap();
        stream.write_all(bytes).await.unwrap();
        let mut answer = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
            .await
            .expect("the proxy ends the connection")
            .unwrap();
        String::from_utf8_lossy(&answer).into_owned()
    }

    /// `CONNECT` to `authority`, with the header a `connect` command sets once it saw a banner.
    async fn announce(&mut self, authority: &str) -> String {
        self.exchange(connect(authority, &["x-puddle-protocol: ssh"]).as_bytes())
            .await
    }

    /// Opens a tunnel to `authority`; the stream is positioned after the `200`.
    async fn tunnel(&mut self, authority: &str) -> StreamHandle {
        self.tunnel_with(authority, &[]).await
    }

    /// [`Self::tunnel`] with extra headers on the `CONNECT`.
    async fn tunnel_with(&mut self, authority: &str, headers: &[&str]) -> StreamHandle {
        let mut stream = self.control.open_stream().await.unwrap();
        stream
            .write_all(connect(authority, headers).as_bytes())
            .await
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).await.unwrap();
            head.extend_from_slice(&byte);
        }
        assert!(
            head.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&head)
        );
        stream
    }
}

fn connect(authority: &str, headers: &[&str]) -> String {
    let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    head
}

/// A server that records what it receives and answers nothing.
struct Silent {
    addr: SocketAddr,
    received: Arc<Mutex<Vec<u8>>>,
    connections: Arc<Mutex<u32>>,
}

impl Silent {
    async fn start() -> Self {
        let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let connections = Arc::new(Mutex::new(0));
        let (r, c) = (received.clone(), connections.clone());
        tokio::spawn(async move {
            loop {
                let (mut conn, _) = listener.accept().await.unwrap();
                *c.lock().unwrap_or_else(PoisonError::into_inner) += 1;
                let r = r.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 1024];
                    while let Ok(n) = conn.read(&mut buf).await {
                        if n == 0 {
                            break;
                        }
                        r.lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .extend_from_slice(buf.get(..n).unwrap_or_default());
                    }
                });
            }
        });
        Self {
            addr,
            received,
            connections,
        }
    }

    fn received(&self) -> Vec<u8> {
        self.received
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn connections(&self) -> u32 {
        *self
            .connections
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// Every event of an SSH refusal says the same, and none of them is a rule or a pending row.
fn assert_ssh_refusals(events: &[ConnectionEvent], count: usize) {
    assert_eq!(events.len(), count, "{events:?}");
    for event in events {
        assert_eq!(event.decision, ConnectionDecision::Blocked, "{event:?}");
        assert_eq!(
            event.reason,
            ConnectionReason::Blocked(BlockReason::SshUnsupported),
            "{event:?}"
        );
        assert_eq!(event.pending_id, None, "{event:?}");
    }
}

#[tokio::test]
async fn an_announced_ssh_connect_to_a_git_host_is_refused_on_any_port_before_any_rule() {
    let rig = Rig::new(&[]);
    // A rule that would allow the host does not matter: SSH is refused before the rules.
    rig.policy.allow(&host("github.com"));
    let mut guest = rig.guest().await;
    let authorities = [
        "github.com:22",
        "ssh.github.com:443",
        "gitlab.com:22",
        "bitbucket.org:22",
        "ssh.dev.azure.com:22",
        "vs-ssh.visualstudio.com:22",
        "altssh.gitlab.com:443",
        "altssh.bitbucket.org:443",
        "github.com:2222",
    ];
    for authority in authorities {
        let answer = guest.announce(authority).await;
        assert!(answer.starts_with("HTTP/1.1 403"), "{authority}: {answer}");
        assert!(
            answer.contains("x-puddle-decision: blocked\r\n")
                && answer.contains("x-puddle-blocked: ssh_unsupported\r\n"),
            "{authority}: {answer}"
        );
        assert!(
            answer.contains("puddle: SSH is not supported yet"),
            "{authority}: {answer}"
        );
    }
    assert_eq!(rig.policy.decisions(), 0, "the rules were never asked");
    assert_eq!(rig.policy.pending().len(), 0, "nothing in the inbox");
    assert_eq!(rig.resolver.lookups(), 0, "nothing was resolved");
    assert_ssh_refusals(&rig.events(authorities.len()).await, authorities.len());
}

/// A probe that answers as set and counts how often it was asked.
struct FixedProbe {
    reachable: bool,
    asked: std::sync::atomic::AtomicUsize,
}

impl FixedProbe {
    fn answering(reachable: bool) -> Arc<Self> {
        Arc::new(Self {
            reachable,
            asked: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn asked(&self) -> usize {
        self.asked.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl PortProbe for FixedProbe {
    fn reachable<'a>(&'a self, _host: &'a str, _port: u16) -> BoxFuture<'a, bool> {
        self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async move { self.reachable })
    }
}

const AZURE_BLOCKED: &str = "puddle: your company network blocks SSH to ssh.dev.azure.com:22; HTTPS remotes work and need no key: Puddle adds your credentials\n";

#[tokio::test]
async fn ssh_to_azure_devops_names_the_company_network_when_the_hosts_probe_of_port_22_failed() {
    let probe = FixedProbe::answering(false);
    let rig = Rig::with_proxy(&[], |p| p.with_port_probe(probe.clone()));
    let mut guest = rig.guest().await;
    let answer = guest.announce("ssh.dev.azure.com:22").await;
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
    assert!(
        answer.contains("x-puddle-blocked: ssh_unsupported\r\n"),
        "{answer}"
    );
    assert!(answer.ends_with(AZURE_BLOCKED), "{answer}");
    assert_eq!(probe.asked(), 1);
    assert_eq!(rig.policy.decisions(), 0, "the rules were never asked");
    assert_ssh_refusals(&rig.events(1).await, 1);
}

#[tokio::test]
async fn ssh_to_azure_devops_keeps_the_ordinary_refusal_when_the_port_is_reachable() {
    let probe = FixedProbe::answering(true);
    let rig = Rig::with_proxy(&[], |p| p.with_port_probe(probe.clone()));
    let mut guest = rig.guest().await;
    let answer = guest.announce("ssh.dev.azure.com:22").await;
    assert!(
        answer.ends_with(
            "puddle: SSH is not supported yet, use HTTPS: the remote becomes https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO\n"
        ),
        "{answer}"
    );
    assert_eq!(probe.asked(), 1);
}

#[tokio::test]
async fn only_azure_devops_on_port_22_is_probed() {
    let probe = FixedProbe::answering(false);
    let rig = Rig::with_proxy(&[], |p| p.with_port_probe(probe.clone()));
    let mut guest = rig.guest().await;
    for authority in [
        "ssh.dev.azure.com:443",
        "vs-ssh.visualstudio.com:22",
        "github.com:22",
        "gitlab.com:22",
    ] {
        let answer = guest.announce(authority).await;
        assert!(
            answer.contains("puddle: SSH is not supported yet"),
            "{authority}: {answer}"
        );
        assert!(!answer.contains("company network"), "{authority}: {answer}");
    }
    assert_eq!(probe.asked(), 0, "no probe for any other destination");
}

#[tokio::test]
async fn without_a_probe_azure_devops_gets_the_ordinary_refusal() {
    let rig = Rig::new(&[]);
    let mut guest = rig.guest().await;
    let answer = guest.announce("ssh.dev.azure.com:22").await;
    assert!(!answer.contains("company network"), "{answer}");
}

#[tokio::test]
async fn the_refusal_gives_the_https_form_of_a_known_git_hosts_remote() {
    let rig = Rig::new(&[]);
    let mut guest = rig.guest().await;
    for (authority, https) in [
        ("github.com:22", "https://github.com/OWNER/REPO.git"),
        ("ssh.github.com:443", "https://github.com/OWNER/REPO.git"),
        ("gitlab.com:22", "https://gitlab.com/GROUP/PROJECT.git"),
        (
            "bitbucket.org:22",
            "https://bitbucket.org/WORKSPACE/REPO.git",
        ),
        (
            "ssh.dev.azure.com:22",
            "https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO",
        ),
        (
            "vs-ssh.visualstudio.com:22",
            "https://dev.azure.com/ORGANIZATION/PROJECT/_git/REPO",
        ),
    ] {
        let answer = guest.announce(authority).await;
        assert!(
            answer.ends_with(&format!(
                "puddle: SSH is not supported yet, use HTTPS: the remote becomes {https}\n"
            )),
            "{authority}: {answer}"
        );
    }
}

#[tokio::test]
async fn the_banner_sent_with_the_connect_is_enough_and_the_header_is_not_case_sensitive() {
    let rig = Rig::new(&[]);
    let mut guest = rig.guest().await;
    let mut early = connect("ssh.github.com:443", &[]).into_bytes();
    early.extend_from_slice(BANNER);
    let answer = guest.exchange(&early).await;
    assert!(
        answer.starts_with("HTTP/1.1 403") && answer.contains("x-puddle-blocked: ssh_unsupported"),
        "{answer}"
    );
    let answer = guest
        .exchange(connect("github.com:22", &["X-Puddle-Protocol:  SSH "]).as_bytes())
        .await;
    assert!(
        answer.contains("x-puddle-blocked: ssh_unsupported"),
        "{answer}"
    );
    assert_eq!(rig.policy.decisions(), 0);
    assert_eq!(rig.policy.pending().len(), 0);
    assert_ssh_refusals(&rig.events(2).await, 2);
}

#[tokio::test]
async fn ssh_to_an_address_whose_toggle_is_off_is_a_toggle_refusal_like_any_connection() {
    let rig = Rig::with_default_guard();
    let mut guest = rig.guest().await;
    for authority in ["127.0.0.1:22", "10.1.2.3:22", "localhost:22"] {
        let answer = guest.announce(authority).await;
        assert!(
            answer.contains("x-puddle-blocked: ") && !answer.contains("ssh_unsupported"),
            "{authority}: {answer}"
        );
        assert!(answer.contains("toggle"), "{authority}: {answer}");
    }
    let events = rig.events(3).await;
    assert!(
        events
            .iter()
            .all(|e| e.decision == ConnectionDecision::Blocked
                && e.reason != ConnectionReason::Blocked(BlockReason::SshUnsupported)),
        "{events:?}"
    );
}

#[tokio::test]
async fn ssh_to_a_name_no_rule_covers_waits_in_the_inbox_with_its_port() {
    let rig = Rig::new(&["nas.test"]);
    let mut guest = rig.guest().await;
    let answer = guest.announce("nas.test:2222").await;
    assert!(
        answer.contains("x-puddle-decision: pending") && !answer.contains("ssh_unsupported"),
        "{answer}"
    );
    let pending = rig.policy.pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].host.to_string(), "nas.test");
    assert_eq!(pending[0].port, 2222);
    let events = rig.events(1).await;
    assert_eq!(events[0].decision, ConnectionDecision::Pending);
    assert_eq!(events[0].pending_id, Some(pending[0].id));
}

#[tokio::test]
async fn ssh_to_a_name_a_rule_denies_is_refused_by_the_rule_not_as_unsupported() {
    let rig = Rig::new(&["nas.test"]);
    rig.policy.deny_host(&host("nas.test"));
    let mut guest = rig.guest().await;
    let answer = guest.announce("nas.test:22").await;
    assert!(
        answer.contains("x-puddle-decision: deny") && !answer.contains("ssh_unsupported"),
        "{answer}"
    );
    assert_eq!(rig.policy.pending().len(), 0);
}

#[tokio::test]
async fn an_allowed_name_gets_its_ssh_spliced_end_to_end_on_any_port() {
    let server = Silent::start().await;
    let rig = Rig::new(&["nas.test"]);
    rig.policy.allow(&host("nas.test"));
    let mut guest = rig.guest().await;
    // Announced, as the agent's `connect` command sends it, and not announced (the banner only
    // follows the 200): the same splice, and the server gets the client's bytes unchanged.
    for headers in [&["x-puddle-protocol: ssh"][..], &[][..]] {
        let mut tunnel = guest
            .tunnel_with(&format!("nas.test:{}", server.addr.port()), headers)
            .await;
        tunnel.write_all(BANNER).await.unwrap();
        tunnel.flush().await.unwrap();
        tunnel.shutdown().await.unwrap();
    }
    // The banner right behind the head, as the agent's `connect` sends it.
    let mut early = guest.control.open_stream().await.unwrap();
    let mut head = connect(
        &format!("nas.test:{}", server.addr.port()),
        &["x-puddle-protocol: ssh"],
    )
    .into_bytes();
    head.extend_from_slice(BANNER);
    early.write_all(&head).await.unwrap();
    puddle_test_kit::eventually(
        "the server received the three banners",
        Duration::from_secs(5),
        || {
            let got = server.received().len();
            if got >= BANNER.len() * 3 {
                Ok(())
            } else {
                Err(format!("{got} of {} bytes", BANNER.len() * 3))
            }
        },
    )
    .await;
    assert_eq!(server.received(), [BANNER, BANNER, BANNER].concat());
    assert_eq!(server.connections(), 3, "one upstream connection each");
    let events = rig.events(2).await;
    for event in &events {
        assert_eq!(event.decision, ConnectionDecision::Allow, "{event:?}");
        assert_eq!(event.reason, ConnectionReason::Rule, "{event:?}");
    }
}

/// The refusal a guest gets for SSH to `gitlab.com` inside an allowed tunnel.
const GITLAB_REFUSAL: &str = "puddle: SSH is not supported yet, use HTTPS: the remote becomes https://gitlab.com/GROUP/PROJECT.git\r\n";

/// Reads `tunnel` to its end; a tunnel the proxy keeps open fails the test instead of hanging it.
async fn read_to_end(tunnel: &mut StreamHandle) -> String {
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), tunnel.read_to_end(&mut answer))
        .await
        .expect("the proxy ends the tunnel")
        .unwrap();
    String::from_utf8(answer).unwrap()
}

#[tokio::test]
async fn hostile_hg58_ssh_to_a_git_host_split_into_small_writes_inside_an_allowed_tunnel_is_still_refused()
 {
    let server = Silent::start().await;
    let rig = Rig::new(&["gitlab.com"]);
    rig.policy.allow(&host("gitlab.com"));
    let mut guest = rig.guest().await;
    let mut tunnel = guest
        .tunnel(&format!("gitlab.com:{}", server.addr.port()))
        .await;
    let (head, rest) = BANNER.split_at(1);
    let (middle, rest) = rest.split_at(3);
    // flake-ok: a send loop, not a wait for a condition: it writes each chunk of a split banner.
    for chunk in [head, middle, rest] {
        tunnel.write_all(chunk).await.unwrap();
        tunnel.flush().await.unwrap();
        // flake-ok: paces the writes so each tends to be its own read at the proxy; the refusal must not depend on it.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(read_to_end(&mut tunnel).await, GITLAB_REFUSAL);
    assert_eq!(
        server.received(),
        b"",
        "nothing of the banner reached the server"
    );
}

#[tokio::test]
async fn hostile_hg58_ssh_to_a_git_host_with_one_byte_behind_the_connect_head_is_still_refused() {
    let server = Silent::start().await;
    let rig = Rig::new(&["gitlab.com"]);
    rig.policy.allow(&host("gitlab.com"));
    let mut guest = rig.guest().await;
    let mut tunnel = guest.control.open_stream().await.unwrap();
    let mut first = connect(&format!("gitlab.com:{}", server.addr.port()), &[]).into_bytes();
    let (early, rest) = BANNER.split_at(1);
    first.extend_from_slice(early);
    tunnel.write_all(&first).await.unwrap();
    tunnel.flush().await.unwrap();
    let mut answer = [0u8; 12];
    tunnel.read_exact(&mut answer).await.unwrap();
    assert_eq!(&answer, b"HTTP/1.1 200");
    tunnel.write_all(rest).await.unwrap();
    tunnel.flush().await.unwrap();
    let answer = read_to_end(&mut tunnel).await;
    assert!(answer.ends_with(GITLAB_REFUSAL), "{answer:?}");
    assert_eq!(
        server.received(),
        b"",
        "nothing of the banner reached the server"
    );
}

#[tokio::test]
async fn first_bytes_to_a_git_host_that_only_start_like_a_banner_pass_unchanged() {
    // The gate holds a prefix of `SSH-` until the next byte tells; what turns out not to be a
    // banner, or ends early, reaches the server whole and in order.
    let server = Silent::start().await;
    let rig = Rig::new(&["gitlab.com"]);
    rig.policy.allow(&host("gitlab.com"));
    let mut guest = rig.guest().await;
    let authority = format!("gitlab.com:{}", server.addr.port());
    let sent: [&[&[u8]]; 3] = [&[b"SS", b"H-x rest"], &[b"S", b"QL"], &[b"SSH-"]];
    let mut expected = Vec::new();
    for chunks in sent {
        let mut tunnel = guest.tunnel(&authority).await;
        // flake-ok: a send loop, not a wait for a condition: it writes each chunk of a split upload.
        for chunk in chunks {
            tunnel.write_all(chunk).await.unwrap();
            tunnel.flush().await.unwrap();
            // flake-ok: paces the writes so each tends to be its own read at the proxy; the bytes must pass either way.
            tokio::time::sleep(Duration::from_millis(30)).await;
            expected.extend_from_slice(chunk);
        }
        tunnel.shutdown().await.unwrap();
        let want = expected.len();
        puddle_test_kit::eventually(
            "the server received the bytes",
            Duration::from_secs(5),
            || {
                let got = server.received().len();
                if got >= want {
                    Ok(())
                } else {
                    Err(format!("{got} of {want} bytes"))
                }
            },
        )
        .await;
    }
    assert_eq!(server.received(), expected);
}

#[tokio::test]
async fn ssh_inside_an_allowed_tunnel_to_a_git_host_gets_the_https_form_too() {
    let server = Silent::start().await;
    // Any name that resolves to the server stands in for the Git host's port 22.
    let rig = Rig::new(&["gitlab.com"]);
    rig.policy.allow(&host("gitlab.com"));
    let mut guest = rig.guest().await;
    let mut tunnel = guest
        .tunnel(&format!("gitlab.com:{}", server.addr.port()))
        .await;
    tunnel.write_all(BANNER).await.unwrap();
    let mut answer = Vec::new();
    tunnel.read_to_end(&mut answer).await.unwrap();
    assert_eq!(
        String::from_utf8(answer).unwrap(),
        "puddle: SSH is not supported yet, use HTTPS: the remote becomes https://gitlab.com/GROUP/PROJECT.git\r\n"
    );
    assert_eq!(server.received().len(), 0);
}

#[tokio::test]
async fn tunnels_that_are_not_ssh_pass_every_byte_unchanged() {
    let server = Silent::start().await;
    let rig = Rig::new(&["svc.test"]);
    rig.policy.allow(&host("svc.test"));
    let mut guest = rig.guest().await;
    let authority = format!("svc.test:{}", server.addr.port());
    // The first chunk decides: TLS, plain text, a banner-like prefix that is too short or has no
    // version, and an SSH identification line that is not the first thing sent.
    let sent: Vec<Vec<u8>> = vec![
        b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03".to_vec(),
        b"hello ".to_vec(),
        b"SSH-".to_vec(),
        b"SSH-x".to_vec(),
        BANNER.to_vec(),
    ];
    let mut tunnel = guest.tunnel(&authority).await;
    let mut expected = Vec::new();
    // flake-ok: paces the chunks so each reaches the proxy as its own first read, not a wait for a condition.
    for chunk in &sent {
        tunnel.write_all(chunk).await.unwrap();
        tunnel.flush().await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        expected.extend_from_slice(chunk);
    }
    tunnel.shutdown().await.unwrap();
    puddle_test_kit::eventually(
        "the server received every sent byte",
        Duration::from_secs(5),
        || {
            let got = server.received().len();
            if got >= expected.len() {
                Ok(())
            } else {
                Err(format!("{got} of {} bytes", expected.len()))
            }
        },
    )
    .await;
    assert_eq!(server.received(), expected);
    // The ones that are not SSH are not recorded as refusals.
    let events = rig.events(1).await;
    assert_eq!(events.first().unwrap().decision, ConnectionDecision::Allow);
}

#[tokio::test]
async fn a_hint_that_is_not_ssh_changes_nothing() {
    let server = Silent::start().await;
    let rig = Rig::new(&["svc.test"]);
    rig.policy.allow(&host("svc.test"));
    let mut guest = rig.guest().await;
    let authority = format!("svc.test:{}", server.addr.port());
    for value in ["telnet", "", "ssh2"] {
        let mut stream = guest.control.open_stream().await.unwrap();
        stream
            .write_all(connect(&authority, &[&format!("x-puddle-protocol: {value}")]).as_bytes())
            .await
            .unwrap();
        let mut first = [0u8; 12];
        stream.read_exact(&mut first).await.unwrap();
        assert_eq!(&first, b"HTTP/1.1 200", "{value:?}");
    }
    assert_eq!(rig.policy.pending().len(), 0);
}

#[tokio::test]
async fn an_unannounced_connect_to_port_22_is_an_ordinary_destination_until_it_speaks_ssh() {
    // Port 22 alone does not make a connection SSH: a name nothing allows is pending as any
    // other, and an allowed one is a tunnel that ends only when its first bytes are SSH.
    let rig = Rig::new(&["other.test"]);
    let mut guest = rig.guest().await;
    let answer = guest
        .exchange(connect("other.test:22", &[]).as_bytes())
        .await;
    assert!(
        answer.contains("x-puddle-decision: pending"),
        "not blocked as SSH: {answer}"
    );
    assert_eq!(rig.policy.pending().len(), 1);
}

#[tokio::test]
async fn the_hosts_direct_ssh_into_a_workspace_needs_still_pass_https_untouched() {
    // Direct SSH into a workspace (an editor's Remote-SSH) is Puddle's own endpoint and never
    // reaches this proxy. What the editor's server does inside the workspace afterwards is HTTPS to
    // the hosts the direct-SSH setting allows: the refusal of the workspace's own SSH leaves it
    // alone.
    let server = Silent::start().await;
    let hosts = [
        "update.code.visualstudio.com",
        "vscode.download.prss.microsoft.com",
        "marketplace.visualstudio.com",
        "ms-vscode.gallery.vsassets.io",
        "ms-vscode.gallerycdn.vsassets.io",
    ];
    let rig = Rig::new(&hosts);
    for h in hosts {
        rig.policy.allow(&host(h));
    }
    let mut guest = rig.guest().await;
    let hello = b"\x16\x03\x01\x02\x00\x01\x00\x01\xfc\x03\x03client hello bytes";
    for h in hosts {
        let mut tunnel = guest.tunnel(&format!("{h}:{}", server.addr.port())).await;
        tunnel.write_all(hello).await.unwrap();
        tunnel.flush().await.unwrap();
    }
    puddle_test_kit::eventually(
        "the server received every hello",
        Duration::from_secs(5),
        || {
            let got = server.received().len();
            let want = hello.len() * hosts.len();
            if got >= want {
                Ok(())
            } else {
                Err(format!("{got} of {want} bytes"))
            }
        },
    )
    .await;
    assert_eq!(server.received().len(), hello.len() * hosts.len());
}

/// Resolves every name to one address, after a pause.
struct SlowResolver(SocketAddr);

impl Resolver for SlowResolver {
    fn resolve<'a>(
        &'a self,
        _name: &'a puddle_types::DomainName,
        _port: u16,
    ) -> BoxFuture<'a, std::io::Result<Vec<SocketAddr>>> {
        Box::pin(async move {
            // Simulated slow resolver.
            tokio::time::sleep(Duration::from_millis(300)).await;
            Ok(vec![self.0])
        })
    }
}

#[tokio::test]
async fn a_guest_that_leaves_before_the_200_ends_its_tunnel_quietly() {
    let server = Silent::start().await;
    let policy = Arc::new(StaticPolicy::new());
    policy.allow(&host("slow.test"));
    let log = Arc::new(CollectingConnectionLog::new());
    let proxy = Arc::new(
        Proxy::new(policy, Arc::new(NullSink))
            .with_connection_log(log.clone())
            .with_resolver(Arc::new(SlowResolver(server.addr)))
            .with_address_check(Arc::new(AnyAddress)),
    );
    let root = IpcRoot::new().unwrap();
    let route = proxy.serve_route(root.listen().unwrap(), WorkspaceId::new("box").unwrap());
    let conn = puddle_ipc::connect(route.endpoint().path()).await.unwrap();
    let mut session = Session::new_client(conn, client_config());
    let mut control = session.control();
    let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
    let mut stream = control.open_stream().await.unwrap();
    stream
        .write_all(connect("slow.test:22", &[]).as_bytes())
        .await
        .unwrap();
    // Gone while the proxy still resolves: the 200 has nobody to go to.
    drop(stream);
    let events = log.wait_for(1, Duration::from_secs(5)).await;
    assert_eq!(events.first().unwrap().decision, ConnectionDecision::Allow);
    assert_eq!(server.received().len(), 0);
    driver.abort();
}

mod filtered_agent {
    //! The filtered SSH agent end to end: a spliced SSH connection to a host the agent is on for
    //! shows its key exchange, and an `ssh-agent` stream of the same workspace binds to that
    //! session and gets a login signed. A Git host's key is refused however the session looks.

    use std::collections::HashMap;

    use puddle_agent_proto::kind::StreamKind;
    use puddle_ca::CaBuilder;
    use puddle_proxy::ssh_agent::{HostAgent, SshAgents};
    use puddle_proxy::{NoInjection, Termination, TerminationSet, Terminations};
    use puddle_ssh_agent::filter::AllowedKeys;
    use puddle_ssh_agent::request;
    use puddle_ssh_agent::testing::{
        TestKey, bind, framed, login, packet, server_kex, sign_request, string,
    };
    use puddle_ssh_agent::{AgentError, Identity};

    use super::*;

    const SESSION: [u8; 32] = [0x5e; 32];
    const GITHUB_ED25519: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl";

    struct FakeAgent {
        user: Vec<u8>,
        signed: Mutex<Vec<Vec<u8>>>,
    }

    impl HostAgent for FakeAgent {
        fn list(&self) -> Result<Vec<Identity>, AgentError> {
            Ok(vec![Identity::new(self.user.clone(), "me@desk".into())])
        }

        fn sign(&self, _key: &[u8], data: &[u8], _flags: u32) -> Result<Vec<u8>, AgentError> {
            self.signed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(data.to_vec());
            Ok(b"SIGNATURE".to_vec())
        }
    }

    /// A server that sends `kex` on every connection and keeps it open.
    async fn kex_server(kex: Vec<u8>) -> SocketAddr {
        let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut conn, _) = listener.accept().await.unwrap();
                let kex = kex.clone();
                tokio::spawn(async move {
                    conn.write_all(&kex).await.unwrap();
                    let mut buf = [0u8; 1024];
                    while matches!(conn.read(&mut buf).await, Ok(n) if n > 0) {}
                });
            }
        });
        addr
    }

    struct AgentRig {
        agents: Arc<SshAgents>,
        agent: Arc<FakeAgent>,
        user: TestKey,
        guest: Guest,
        _route: Route,
        _root: IpcRoot,
    }

    /// Workspace `box` may reach `nas.test`, and has the agent on for it with every key.
    async fn agent_rig() -> AgentRig {
        agent_rig_with(&puddle_proxy::ProxyConfig::default()).await
    }

    async fn agent_rig_with(config: &puddle_proxy::ProxyConfig) -> AgentRig {
        agent_rig_allowing(config, AllowedKeys::All).await
    }

    async fn agent_rig_allowing(config: &puddle_proxy::ProxyConfig, keys: AllowedKeys) -> AgentRig {
        let policy = Arc::new(StaticPolicy::new());
        policy.allow(&host("nas.test"));
        let user = TestKey::ed25519(2);
        let agent = Arc::new(FakeAgent {
            user: user.blob(),
            signed: Mutex::new(Vec::new()),
        });
        let agents = Arc::new(SshAgents::new(agent.clone()));
        let terminations = Arc::new(Terminations::new());
        let ca = Arc::new(CaBuilder::new("test").build().unwrap());
        terminations.insert(
            WorkspaceId::new("box").unwrap(),
            Termination::new(TerminationSet::new(), ca, Arc::new(NoInjection))
                .with_ssh_agent_hosts(HashMap::from([(host("nas.test"), keys)])),
        );
        let proxy = Arc::new(
            Proxy::new(policy, Arc::new(NullSink))
                .with_resolver(Arc::new(StaticResolver::new().with("nas.test", &[LOCAL])))
                .with_address_check(Arc::new(AnyAddress))
                .with_config(*config)
                .with_termination(terminations, puddle_upstream::TlsClient::new([]).unwrap())
                .with_ssh_agent(Arc::clone(&agents)),
        );
        let root = IpcRoot::new().unwrap();
        let route = proxy.serve_route(root.listen().unwrap(), WorkspaceId::new("box").unwrap());
        let conn = puddle_ipc::connect(route.endpoint().path()).await.unwrap();
        let mut session = Session::new_client(conn, client_config());
        let control = session.control();
        let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
        AgentRig {
            agents,
            agent,
            user,
            guest: Guest { control, driver },
            _route: route,
            _root: root,
        }
    }

    impl AgentRig {
        /// An announced SSH tunnel to the server at `addr`, read up to the end of `kex`, and the
        /// filtered agent has the session.
        async fn spliced(&mut self, addr: SocketAddr, kex: &[u8]) -> StreamHandle {
            let mut tunnel = self
                .guest
                .tunnel_with(
                    &format!("nas.test:{}", addr.port()),
                    &["x-puddle-protocol: ssh"],
                )
                .await;
            tunnel.write_all(BANNER).await.unwrap();
            let mut seen = vec![0u8; kex.len()];
            tokio::time::timeout(Duration::from_secs(5), tunnel.read_exact(&mut seen))
                .await
                .expect("the server's key exchange reaches the guest")
                .unwrap();
            assert_eq!(seen, kex, "the guest gets the server's bytes unchanged");
            assert_eq!(self.agents.seen_sessions(), 1);
            tunnel
        }

        async fn agent_stream(&mut self) -> StreamHandle {
            let mut stream = self.guest.control.open_stream().await.unwrap();
            stream
                .write_all(StreamKind::SshAgent.preamble())
                .await
                .unwrap();
            stream
        }
    }

    /// The proxy closes `stream` without an answer.
    async fn closed(mut stream: StreamHandle) {
        let mut rest = Vec::new();
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut rest))
            .await
            .expect("the proxy closes the stream");
        assert!(read.is_err() || rest.is_empty(), "{read:?} {rest:?}");
    }

    #[tokio::test]
    async fn a_host_without_a_filtered_agent_closes_agent_streams() {
        let rig = Rig::new(&[]);
        let mut guest = rig.guest().await;
        let mut stream = guest.control.open_stream().await.unwrap();
        stream
            .write_all(StreamKind::SshAgent.preamble())
            .await
            .unwrap();
        stream.write_all(&framed(&[11])).await.unwrap();
        closed(stream).await;
    }

    #[tokio::test]
    async fn an_agent_stream_over_the_stream_limit_is_closed_and_a_finished_one_frees_its_place() {
        let mut rig =
            agent_rig_with(&puddle_proxy::ProxyConfig::default().with_max_streams_per_workspace(1))
                .await;
        let mut first = rig.agent_stream().await;
        assert_eq!(ask(&mut first, &[11]).await, request::identities(&[]));
        let mut second = rig.agent_stream().await;
        second.write_all(&framed(&[11])).await.unwrap();
        closed(second).await;
        // The guest ends the first; the proxy ends its side too and the place is free again.
        first.shutdown().await.unwrap();
        closed(first).await;
        let mut third = rig.agent_stream().await;
        assert_eq!(ask(&mut third, &[11]).await, request::identities(&[]));
    }

    async fn ask(stream: &mut StreamHandle, message: &[u8]) -> Vec<u8> {
        stream.write_all(&framed(message)).await.unwrap();
        let mut len = [0u8; 4];
        tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut len))
            .await
            .expect("the filtered agent answers")
            .unwrap();
        let mut body = vec![0u8; u32::from_be_bytes(len) as usize];
        stream.read_exact(&mut body).await.unwrap();
        framed(&body)
    }

    #[tokio::test]
    async fn an_agent_stream_binds_to_a_spliced_session_and_gets_its_login_signed() {
        let server = TestKey::ed25519(1);
        let kex = server_kex(&server, &server.sign(&SESSION));
        let addr = kex_server(kex.clone()).await;
        let mut rig = agent_rig().await;
        let tunnel = rig.spliced(addr, &kex).await;
        let mut agent = rig.agent_stream().await;
        let the_bind = bind(&server.blob(), &SESSION, &server.sign(&SESSION), false);
        assert_eq!(ask(&mut agent, &the_bind).await, request::success());
        let key = rig.user.blob();
        let listed = Identity::new(key.clone(), String::new());
        assert_eq!(
            ask(&mut agent, &[11]).await,
            request::identities(&[&listed])
        );
        let data = login(&SESSION, &key, Some(&server.blob()));
        assert_eq!(
            ask(&mut agent, &sign_request(&key, &data, 0)).await,
            request::signature(b"SIGNATURE")
        );
        assert_eq!(
            *rig.agent
                .signed
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            vec![data]
        );
        // Once the tunnel is gone its session binds nothing.
        drop(tunnel);
        puddle_test_kit::eventually(
            "the closed tunnel's session is forgotten",
            Duration::from_secs(5),
            || match rig.agents.seen_sessions() {
                0 => Ok(()),
                n => Err(format!("{n} kept")),
            },
        )
        .await;
        let mut late = rig.agent_stream().await;
        assert_eq!(ask(&mut late, &the_bind).await, request::failure());
    }

    #[tokio::test]
    async fn a_session_with_a_git_hosts_key_is_refused_even_when_spliced_and_on() {
        use ssh_key::PublicKey;
        let github = PublicKey::from_openssh(GITHUB_ED25519).unwrap();
        // A server that shows GitHub's host key with a well-formed signature that cannot be
        // right: the deny list decides before the signature is checked.
        let signature = [string(b"ssh-ed25519"), string(&[0; 64])].concat();
        let kex = server_kex_with_blob(&github.to_bytes().unwrap(), &signature);
        let addr = kex_server(kex.clone()).await;
        let mut rig = agent_rig().await;
        let _tunnel = rig.spliced(addr, &kex).await;
        let mut agent = rig.agent_stream().await;
        let the_bind = bind(&github.to_bytes().unwrap(), &SESSION, &signature, false);
        assert_eq!(ask(&mut agent, &the_bind).await, request::failure());
        assert_eq!(
            ask(&mut agent, &[11]).await,
            request::identities(&[]),
            "nothing listed without a bind"
        );
        assert!(
            rig.agent
                .signed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
    }

    /// What a hostile guest asks the agent in the hope of a signature: each must be answered with
    /// failure and reach the user's agent never.
    async fn refused_all(stream: &mut StreamHandle, messages: &[Vec<u8>]) {
        for message in messages {
            assert_eq!(
                ask(stream, message).await,
                request::failure(),
                "{message:02x?}"
            );
        }
    }

    fn never_signed(rig: &AgentRig) {
        assert!(
            rig.agent
                .signed
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty(),
            "the user's agent signed nothing"
        );
    }

    #[tokio::test]
    async fn hostile_hg52_without_a_bind_or_with_a_bind_for_a_session_that_never_crossed_puddle_nothing_is_signed()
     {
        let server = TestKey::ed25519(1);
        let kex = server_kex(&server, &server.sign(&SESSION));
        let addr = kex_server(kex.clone()).await;
        let mut rig = agent_rig().await;
        let _tunnel = rig.spliced(addr, &kex).await;
        let key = rig.user.blob();
        let data = login(&SESSION, &key, Some(&server.blob()));
        let mut agent = rig.agent_stream().await;
        // No bind at all: the list is empty and the login is refused.
        assert_eq!(ask(&mut agent, &[11]).await, request::identities(&[]));
        refused_all(&mut agent, &[sign_request(&key, &data, 0)]).await;
        // A bind for a session of another server, one that Puddle never saw: for the jump
        // host's inner session, a key of the guest's own, and a real host key with a session id
        // its signature does not cover.
        let other = TestKey::ed25519(9);
        let other_session = [0x77; 32];
        let foreign = [
            bind(
                &other.blob(),
                &other_session,
                &other.sign(&other_session),
                false,
            ),
            bind(
                &server.blob(),
                &other_session,
                &server.sign(&SESSION),
                false,
            ),
            bind(&server.blob(), &SESSION, &other.sign(&SESSION), false),
            bind(&server.blob(), &SESSION, &[], false),
            bind(&[], &SESSION, &server.sign(&SESSION), false),
        ];
        refused_all(&mut agent, &foreign).await;
        assert_eq!(
            ask(&mut agent, &[11]).await,
            request::identities(&[]),
            "a refused bind lists nothing"
        );
        refused_all(&mut agent, &[sign_request(&key, &data, 0)]).await;
        // A bind that forwards, and a second bind on the way to a real one, do not help either.
        let forwarding = bind(&server.blob(), &SESSION, &server.sign(&SESSION), true);
        refused_all(&mut agent, &[forwarding]).await;
        refused_all(&mut agent, &[sign_request(&key, &data, 0)]).await;
        never_signed(&rig);
    }

    #[tokio::test]
    async fn hostile_hg52_a_bound_session_still_gets_no_commit_signature_no_edit_and_no_other_keys()
    {
        let server = TestKey::ed25519(1);
        let kex = server_kex(&server, &server.sign(&SESSION));
        let addr = kex_server(kex.clone()).await;
        let mut rig = agent_rig().await;
        let _tunnel = rig.spliced(addr, &kex).await;
        let mut agent = rig.agent_stream().await;
        let the_bind = bind(&server.blob(), &SESSION, &server.sign(&SESSION), false);
        assert_eq!(ask(&mut agent, &the_bind).await, request::success());
        let key = rig.user.blob();
        let sshsig = |namespace: &[u8]| {
            [
                b"SSHSIG".as_slice(),
                &[0, 0, 0, 1],
                &string(namespace),
                &string(b""),
                &string(b"sha512"),
            ]
            .concat()
        };
        let mut extension = vec![27];
        extension.extend(string(b"query"));
        let mut hostile = vec![
            sign_request(&key, &sshsig(b"git"), 0),
            sign_request(&key, &sshsig(b"file"), 0),
            sign_request(&key, b"any other data", 0),
            sign_request(&key, &login(&[0x01; 32], &key, Some(&server.blob())), 0),
            sign_request(
                &key,
                &login(&SESSION, &key, Some(&TestKey::ed25519(8).blob())),
                0,
            ),
            sign_request(
                &TestKey::ed25519(7).blob(),
                &login(&SESSION, &key, Some(&server.blob())),
                0,
            ),
            extension,
        ];
        // Add (17, 25), remove (18, 19), lock (22), unlock (23), add-smartcard (20, 26) and
        // constrained keys, each with an empty body.
        hostile.extend([17, 18, 19, 20, 21, 22, 23, 25, 26].map(|kind| vec![kind, 0, 0, 0, 0]));
        refused_all(&mut agent, &hostile).await;
        never_signed(&rig);
        // The connection and its bind survive all of it: the real login still works.
        let data = login(&SESSION, &key, Some(&server.blob()));
        assert_eq!(
            ask(&mut agent, &sign_request(&key, &data, 0)).await,
            request::signature(b"SIGNATURE")
        );
    }

    #[tokio::test]
    async fn hostile_hg52_the_keys_of_another_host_are_neither_listed_nor_signed_with() {
        let server = TestKey::ed25519(1);
        let kex = server_kex(&server, &server.sign(&SESSION));
        let addr = kex_server(kex.clone()).await;
        // The agent holds the user's key; the host may use only another one.
        let only_other = AllowedKeys::Only(std::collections::BTreeSet::from([
            TestKey::ed25519(6).fingerprint()
        ]));
        let mut rig = agent_rig_allowing(&puddle_proxy::ProxyConfig::default(), only_other).await;
        let _tunnel = rig.spliced(addr, &kex).await;
        let mut agent = rig.agent_stream().await;
        let the_bind = bind(&server.blob(), &SESSION, &server.sign(&SESSION), false);
        assert_eq!(ask(&mut agent, &the_bind).await, request::success());
        assert_eq!(ask(&mut agent, &[11]).await, request::identities(&[]));
        let key = rig.user.blob();
        let data = login(&SESSION, &key, Some(&server.blob()));
        refused_all(&mut agent, &[sign_request(&key, &data, 0)]).await;
        never_signed(&rig);
    }

    #[tokio::test]
    async fn hostile_hg53_a_login_to_github_through_the_jump_host_gets_no_signature() {
        use ssh_key::PublicKey;
        // `ssh -J jump.example git@github.com`: the jump host's own session is spliced and the
        // agent is on for it. The inner session to GitHub runs inside a channel of the first, so
        // Puddle never sees its key exchange; the guest's ssh binds to it with GitHub's host key.
        let jump = TestKey::ed25519(1);
        let kex = server_kex(&jump, &jump.sign(&SESSION));
        let addr = kex_server(kex.clone()).await;
        let mut rig = agent_rig().await;
        let _tunnel = rig.spliced(addr, &kex).await;
        let mut agent = rig.agent_stream().await;
        let jump_bind = bind(&jump.blob(), &SESSION, &jump.sign(&SESSION), false);
        assert_eq!(ask(&mut agent, &jump_bind).await, request::success());
        let github = PublicKey::from_openssh(GITHUB_ED25519)
            .unwrap()
            .to_bytes()
            .unwrap();
        let inner_session = [0x1d; 32];
        let key = rig.user.blob();
        let signature = [string(b"ssh-ed25519"), string(&[0; 64])].concat();
        // ssh opens one agent connection per session: the inner session's is a second stream.
        let mut inner = rig.agent_stream().await;
        let inner_bind = bind(&github, &inner_session, &signature, false);
        let inner_login = login(&inner_session, &key, Some(&github));
        refused_all(
            &mut inner,
            &[
                inner_bind.clone(),
                sign_request(&key, &inner_login, 0),
                sign_request(&key, &login(&inner_session, &key, None), 0),
            ],
        )
        .await;
        assert_eq!(ask(&mut inner, &[11]).await, request::identities(&[]));
        // A guest that sends the inner bind down the jump host's own connection loses that
        // binding too: nothing is signed there afterwards, for either session.
        let data = login(&SESSION, &key, Some(&jump.blob()));
        refused_all(&mut agent, &[inner_bind]).await;
        refused_all(
            &mut agent,
            &[
                sign_request(&key, &inner_login, 0),
                sign_request(&key, &data, 0),
            ],
        )
        .await;
        never_signed(&rig);
        // The jump host's own login, the one thing the agent is on for, works on a connection
        // that binds it alone.
        let mut again = rig.agent_stream().await;
        assert_eq!(ask(&mut again, &jump_bind).await, request::success());
        assert_eq!(
            ask(&mut again, &sign_request(&key, &data, 0)).await,
            request::signature(b"SIGNATURE")
        );
        assert_eq!(
            *rig.agent
                .signed
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            vec![data],
            "only the jump host's login was signed"
        );
    }

    #[tokio::test]
    async fn only_an_announced_tunnel_to_a_host_the_agent_is_on_for_is_watched() {
        let server = TestKey::ed25519(1);
        let kex = server_kex(&server, &server.sign(&SESSION));
        let addr = kex_server(kex.clone()).await;
        let mut rig = agent_rig().await;
        // Not announced: the same server, the same host, no session kept.
        let mut tunnel = rig.guest.tunnel(&format!("nas.test:{}", addr.port())).await;
        tunnel.write_all(BANNER).await.unwrap();
        let mut seen = vec![0u8; kex.len()];
        tokio::time::timeout(Duration::from_secs(5), tunnel.read_exact(&mut seen))
            .await
            .expect("the server's bytes reach the guest")
            .unwrap();
        assert_eq!(seen, kex);
        assert_eq!(rig.agents.seen_sessions(), 0);
    }

    /// [`server_kex`] with a host key given as a blob, for a key this test has no private half of.
    fn server_kex_with_blob(host_key: &[u8], signature: &[u8]) -> Vec<u8> {
        let mut out = b"SSH-2.0-TestServer\r\n".to_vec();
        out.extend(packet(&[20; 17]));
        let mut reply = vec![31];
        reply.extend(string(host_key));
        reply.extend(string(&[7; 32]));
        reply.extend(string(signature));
        out.extend(packet(&reply));
        out.extend(packet(&[21]));
        out
    }
}
