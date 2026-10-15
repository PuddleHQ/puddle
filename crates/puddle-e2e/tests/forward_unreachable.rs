// SPDX-License-Identifier: GPL-3.0-or-later
//! A hostile guest tries a port forward of its own workspace through the real proxy: the forward's
//! listener on this computer's loopback is one of Puddle's own, so the agent's `CONNECT` to it is
//! refused with a reason, whatever the loopback toggles and the rules say. Without that, a guest
//! could dial its own forward and come back in at a port of its own it never exposed, or dial
//! another workspace's forward and reach a service that workspace chose to open to the host.
//!
//! The rig is the whole path: a real agent, the real proxy and its address guard (every local
//! category on, wildcards reaching local: the most permissive guard there is), `Ports` as the
//! proxy's guest watcher, and a "guest server" on this machine's loopback that counts the
//! connections it gets. The positive control opens a connection from outside (the host's user) and
//! shows the forward does carry bytes to that server; the attempts from the guest must leave the
//! count where it was, so a pass is not a forward that never worked.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use puddle_agent::config::Target;
use puddle_agent::{Agent, Config};
use puddle_ipc::IpcRoot;
use puddle_netpolicy::{LocalAccess, NetPolicy, PuddleEndpoints};
use puddle_ports::{Pin, Pins, PinsError, Ports};
use puddle_proxy::testing::StaticResolver;
use puddle_proxy::{Proxy, Route};
use puddle_store::{Limits, ManualClock, Store};
use puddle_types::{LocalCategory, NullSink, WorkspaceName};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const NOW: u64 = 1_800_000_000_000;

/// No pins are kept in this rig.
struct NoPins;

impl Pins for NoPins {
    fn pinned(&self, _: &WorkspaceName) -> Result<Vec<Pin>, PinsError> {
        Ok(Vec::new())
    }
    fn all(&self) -> Result<Vec<(WorkspaceName, Pin)>, PinsError> {
        Ok(Vec::new())
    }
    fn pin(&self, _: &WorkspaceName, _: Pin) -> Result<(), PinsError> {
        Ok(())
    }
    fn unpin(&self, _: &WorkspaceName, _: u16) -> Result<bool, PinsError> {
        Ok(false)
    }
}

struct Rig {
    agent: Agent,
    ports: Ports,
    workspace: WorkspaceName,
    /// Connections the guest's own server has accepted.
    accepted: Arc<AtomicUsize>,
    /// The guest server's port.
    guest_port: u16,
    _route: Route,
    _root: IpcRoot,
}

async fn rig() -> Rig {
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open_in_memory(clock, Limits::default()).unwrap());
    let endpoints = PuddleEndpoints::new();
    let ports = Ports::new(endpoints.clone(), Arc::new(NoPins), Arc::new(NullSink));
    let access = LocalCategory::ALL
        .into_iter()
        .fold(LocalAccess::NONE, |a, c| a.with_toggle(c, true))
        .with_wildcards_reach_local(true);
    let guard = NetPolicy::new(Arc::new(access)).with_endpoints(endpoints);
    let proxy = Arc::new(
        Proxy::new(store, Arc::new(NullSink))
            .with_resolver(Arc::new(StaticResolver::new()))
            .with_address_check(Arc::new(guard))
            .with_guest_watcher(Arc::new(ports.clone())),
    );
    let workspace = WorkspaceName::new("e2e-fwd").unwrap();
    let root = IpcRoot::new().unwrap();
    let route = proxy.serve_route(root.listen().unwrap(), workspace.clone());
    let agent = Agent::start(Config {
        listen: SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), 0),
        target: Target::Unix(route.endpoint().path().to_path_buf()),
        oom: None,
        ports: None,
        ..Config::default()
    })
    .await
    .unwrap();

    // The guest's own server: counts what connects, answers a greeting.
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let guest_port = server.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = server.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let _ = conn.write_all(b"hello from the guest").await;
                let _ = conn.shutdown().await;
            });
        }
    });
    Rig {
        agent,
        ports,
        workspace,
        accepted,
        guest_port,
        _route: route,
        _root: root,
    }
}

/// The status line and headers of the answer to `CONNECT authority` through the agent.
async fn connect_via(agent: SocketAddr, authority: &str) -> io::Result<(u16, Vec<String>)> {
    let mut conn = TcpStream::connect(agent).await?;
    conn.write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await?;
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let code = line
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| io::Error::other(format!("bad status line {line:?}")))?;
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).await?;
        if h == "\r\n" || h.is_empty() {
            break;
        }
        headers.push(h.trim_end().to_ascii_lowercase());
    }
    Ok((code, headers))
}

#[tokio::test]
async fn a_guest_cannot_dial_a_forward_of_its_own_workspace_even_with_every_local_toggle_on() {
    let rig = rig().await;
    rig.ports.started(&rig.workspace).await;
    // A port of this computer the forward will take.
    let host_port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let row = rig
        .ports
        .forward(&rig.workspace, rig.guest_port, Some(host_port), false)
        .await
        .unwrap();
    assert!(row.forwarded);

    // Control: from outside the workspace (the host's user) the forward reaches the guest server.
    let mut outside = TcpStream::connect(("127.0.0.1", host_port)).await.unwrap();
    let mut greeting = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), outside.read_to_end(&mut greeting))
        .await
        .expect("the forward did not answer")
        .unwrap();
    assert_eq!(greeting, b"hello from the guest");
    assert_eq!(
        rig.accepted.load(Ordering::SeqCst),
        1,
        "the control reached the guest once"
    );

    // The guest dials the forward every way it can spell the address.
    for authority in [
        format!("127.0.0.1:{host_port}"),
        format!("localhost:{host_port}"),
        format!("[::ffff:127.0.0.1]:{host_port}"),
        format!("0.0.0.0:{host_port}"),
    ] {
        let (code, headers) = connect_via(rig.agent.local_addr(), &authority)
            .await
            .unwrap();
        assert_eq!(code, 403, "{authority}");
        assert!(
            headers.contains(&"x-puddle-blocked: puddle_endpoint".to_owned()),
            "{authority}: {headers:?}"
        );
        assert!(
            !headers.iter().any(|h| h.starts_with("x-puddle-pending")),
            "{authority}: never approvable: {headers:?}"
        );
    }
    // Nothing came through the forward: the guest server was not reached again.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.accepted.load(Ordering::SeqCst),
        1,
        "a refused guest connection still reached the guest server through the forward"
    );
}
