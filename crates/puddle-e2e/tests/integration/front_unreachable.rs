// SPDX-License-Identifier: GPL-3.0-or-later
//! A hostile guest tries the browser editor's front through the real proxy: the front's listener
//! on this computer's loopback is one of Puddle's own, so the agent's `CONNECT` to it is refused
//! with a reason, whatever the loopback toggles and the rules say. Without that, a guest could dial
//! the front with its own workspace's `Host` and come back in at its own editor server from the
//! outside (and with another workspace's name, at that workspace's editor).
//!
//! The rig is the whole path: a real agent that runs an editor server on a port (a plain HTTP
//! server on this machine's loopback that counts what connects stands in for it), the real proxy
//! and its address guard (every local category on, wildcards reaching local: the most permissive
//! guard there is), `Ports` as the proxy's guest watcher and as the front's way into the guest.
//! The positive control makes a request from outside, as the user's window would, and shows the
//! front does carry it to that server; the attempts from the guest must leave the count where it
//! was, so a pass is not a front that never worked. The negative control is a second front that is
//! not in the guard's registry: the same attempt is not refused as a Puddle listener, so the test
//! can fail.
#![expect(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use puddle_agent::config::Target;
use puddle_agent::editor::EditorConfig;
use puddle_agent::{Agent, Config};
use puddle_agent_proto::editor::EditorServer;
use puddle_front::{FileHalves, Flavour, Front, PortsGuests};
use puddle_ipc::IpcRoot;
use puddle_netpolicy::{LocalAccess, NetPolicy, PuddleEndpoints};
use puddle_ports::{MemoryPins, Ports};
use puddle_proxy::testing::StaticResolver;
use puddle_proxy::{Proxy, Route};
use puddle_store::{Limits, ManualClock, Store};
use puddle_types::{LocalCategory, NullSink, WorkspaceName};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

const NOW: u64 = 1_800_000_000_000;
const PASSWORD: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Rig {
    agent: Agent,
    ports: Ports,
    endpoints: PuddleEndpoints,
    workspace: WorkspaceName,
    /// Connections the editor server has accepted.
    accepted: Arc<AtomicUsize>,
    _route: Route,
    _root: IpcRoot,
    _dir: tempfile::TempDir,
}

async fn rig() -> Rig {
    let clock = Arc::new(ManualClock::new(NOW));
    let store = Arc::new(Store::open_in_memory(clock, Limits::default()).unwrap());
    let endpoints = PuddleEndpoints::new();
    let ports = Ports::new(
        endpoints.clone(),
        Arc::new(MemoryPins::default()),
        Arc::new(NullSink),
    );
    let access = LocalCategory::ALL
        .into_iter()
        .fold(LocalAccess::NONE, |a, c| a.with_toggle(c, true))
        .with_wildcards_reach_local(true);
    let guard = NetPolicy::new(Arc::new(access)).with_endpoints(endpoints.clone());
    let proxy = Arc::new(
        Proxy::new(store, Arc::new(NullSink))
            .with_resolver(Arc::new(
                StaticResolver::new()
                    .with("e2e-front.localhost", &[Ipv4Addr::LOCALHOST.into()])
                    .with("other.localhost", &[Ipv4Addr::LOCALHOST.into()]),
            ))
            .with_address_check(Arc::new(guard))
            .with_guest_watcher(Arc::new(ports.clone())),
    );
    let workspace = WorkspaceName::new("e2e-front").unwrap();
    let root = IpcRoot::new().unwrap();
    let route = proxy.serve_route(root.listen().unwrap(), workspace.clone());

    // The editor server: counts what connects, answers one HTTP greeting.
    let server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let editor_port = server.local_addr().unwrap().port();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = server.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let mut head = [0_u8; 1024];
                let _ = conn.read(&mut head).await;
                let _ = conn
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 5\r\nconnection: close\r\n\r\nhello",
                    )
                    .await;
                let _ = conn.shutdown().await;
            });
        }
    });

    // The agent's editor settings point at that port. Its port is taken, so the agent starts no
    // server of its own and the stand-in answers.
    let dir = tempfile::tempdir().unwrap();
    let password_file = dir.path().join("editor-password");
    std::fs::write(&password_file, PASSWORD).unwrap();
    let mut editor = EditorServer::new("/nonexistent/data", "/nonexistent/work", 120);
    // flake-ok: a path that must not exist; on Windows it is rooted on the current drive and still does not exist
    editor.dir = "/nonexistent/release".into();
    editor.port = editor_port;
    let agent = Agent::start(Config {
        listen: SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0),
        target: Target::Unix(route.endpoint().path().to_path_buf()),
        oom: None,
        ports: None,
        editor: Some(EditorConfig {
            server: editor,
            password_file,
        }),
        ..Config::default()
    })
    .await
    .unwrap();
    ports_started(&ports, &workspace).await;
    Rig {
        agent,
        ports,
        endpoints,
        workspace,
        accepted,
        _route: route,
        _root: root,
        _dir: dir,
    }
}

async fn ports_started(ports: &Ports, workspace: &WorkspaceName) {
    ports.started(workspace).await;
    puddle_test_kit::eventually("the agent said hello", Duration::from_secs(5), || {
        ports.guest_link(workspace).is_some()
    })
    .await;
}

async fn front_for(rig: &Rig, endpoints: &PuddleEndpoints) -> (Front, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let front = Front::bind(
        None,
        endpoints,
        Arc::new(PortsGuests::new(rig.ports.clone(), |_| Flavour::CodeServer)),
        Arc::new(FileHalves::new(dir.path())),
    )
    .await
    .unwrap();
    (front, dir)
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

/// What the user's window gets from the front for `workspace`.
async fn from_outside(front: &Front, workspace: &WorkspaceName) -> String {
    let port = front.local_addr().port();
    let mut conn = TcpStream::connect(front.local_addr()).await.unwrap();
    conn.write_all(
        format!(
            "GET / HTTP/1.1\r\nHost: {workspace}.localhost:{port}\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut answer = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), conn.read_to_end(&mut answer))
        .await
        .expect("the front did not answer")
        .unwrap();
    String::from_utf8_lossy(&answer).into_owned()
}

fn spellings(port: u16) -> [String; 6] {
    [
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::ffff:127.0.0.1]:{port}"),
        format!("0.0.0.0:{port}"),
        // The front's own names, as a guest would dial them: a guard that judged the name
        // before resolving it would let these through.
        format!("e2e-front.localhost:{port}"),
        format!("other.localhost:{port}"),
    ]
}

#[tokio::test]
async fn a_guest_cannot_dial_the_front_even_with_every_local_toggle_on() {
    let rig = rig().await;
    let (front, _dir) = front_for(&rig, &rig.endpoints).await;

    // Control: from outside the workspace the front reaches the editor server.
    let answer = from_outside(&front, &rig.workspace).await;
    assert!(
        answer.starts_with("HTTP/1.1 200") && answer.ends_with("hello"),
        "{answer}"
    );
    assert_eq!(
        rig.accepted.load(Ordering::SeqCst),
        1,
        "the control reached the editor once"
    );

    // The guest dials the front every way it can spell the address.
    for authority in spellings(front.local_addr().port()) {
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
    // Absence window: a late connection to the editor would show only after the refusals, and nothing signals one that never comes.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        rig.accepted.load(Ordering::SeqCst),
        1,
        "a refused guest connection still reached the editor server through the front"
    );
}

#[tokio::test]
async fn negative_control_a_front_outside_the_registry_is_not_refused_as_a_puddle_listener() {
    let rig = rig().await;
    // Bound against a registry the guard never reads: the one thing the real front does that
    // keeps the guest out is missing.
    let (front, _dir) = front_for(&rig, &PuddleEndpoints::new()).await;
    let authority = format!("127.0.0.1:{}", front.local_addr().port());
    let (_, headers) = connect_via(rig.agent.local_addr(), &authority)
        .await
        .unwrap();
    assert!(
        !headers.contains(&"x-puddle-blocked: puddle_endpoint".to_owned()),
        "the test cannot tell a registered front from an unregistered one: {headers:?}"
    );
}
