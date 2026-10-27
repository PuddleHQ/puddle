// SPDX-License-Identifier: GPL-3.0-or-later
//! The port forward and the listening-ports report, through a real agent and the host's session
//! code over a Unix socket: the host opens a stream to the agent, the agent connects to a port in
//! "the guest" (this machine's loopback), and the bytes go through both ways.
#![cfg(unix)]
#![expect(
    clippy::panic,
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use crate::common;

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use puddle_agent::config::{PortWatch, Target};
use puddle_agent::{Agent, Config};
use puddle_agent_proto::forward::{GuestLink, OpenError};
use puddle_agent_proto::host::{GuestStream, HostConfig, StreamHandler, serve_session};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UnixListener};

/// What the host side of the session was told.
#[derive(Default)]
struct Told {
    link: Mutex<Option<GuestLink>>,
    ports: Mutex<Vec<Vec<u16>>>,
}

#[derive(Clone)]
struct Host(Arc<Told>);

impl StreamHandler for Host {
    async fn handle(&self, _stream: GuestStream) {}

    async fn guest_link(&self, link: GuestLink) {
        *self.0.link.lock().unwrap() = Some(link);
    }

    async fn guest_ports(&self, ports: Vec<u16>) {
        self.0.ports.lock().unwrap().push(ports);
    }
}

struct Rig {
    told: Arc<Told>,
    agent: Agent,
    tables: std::path::PathBuf,
    _dir: common::TempDir,
}

async fn until<T>(mut look: impl FnMut() -> Option<T>) -> T {
    for _ in 0..600 {
        if let Some(found) = look() {
            return found;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("not seen in time");
}

async fn rig(tag: &str) -> Rig {
    let dir = common::TempDir::new(tag);
    let socket = dir.0.join("route.sock");
    let tables = dir.0.join("net");
    std::fs::create_dir_all(&tables).unwrap();
    let told = Arc::new(Told::default());
    let listener = UnixListener::bind(&socket).unwrap();
    let handler = Host(Arc::clone(&told));
    tokio::spawn(async move {
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            tokio::spawn(async move {
                let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
                let _ = serve_session(
                    conn,
                    common::workspace(),
                    Arc::new(common::ChanSink(tx)),
                    Arc::new(handler),
                    HostConfig::default(),
                )
                .await;
            });
        }
    });
    let config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        target: Target::Unix(socket),
        oom: None,
        ports: Some(PortWatch {
            dir: tables.clone(),
            poll: Duration::from_millis(20),
        }),
        ..Config::default()
    };
    let agent = Agent::start(config).await.unwrap();
    Rig {
        told,
        agent,
        tables,
        _dir: dir,
    }
}

impl Rig {
    async fn link(&self) -> GuestLink {
        until(|| self.told.link.lock().unwrap().clone()).await
    }

    /// Writes the kernel's socket table with a wildcard listener on each of `ports`.
    fn listening(&self, ports: &[u16]) {
        let mut rows = String::new();
        for p in ports {
            writeln!(
                rows,
                "   0: 00000000:{p:04X} 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 1 1 0"
            )
            .unwrap();
        }
        std::fs::write(
            self.tables.join("tcp"),
            format!("  sl  local_address rem_address   st\n{rows}"),
        )
        .unwrap();
    }
}

/// An echo server on this machine's loopback.
async fn echo() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut conn, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    port
}

#[tokio::test]
async fn the_hosts_forward_reaches_a_port_in_the_guest_and_carries_bytes_both_ways() {
    let rig = rig("through").await;
    let link = rig.link().await;
    assert_eq!(format!("{link:?}"), "GuestLink { .. }");
    let port = echo().await;
    let mut stream = link.open_forward(port).await.unwrap();
    // 3 MiB each way, written and read at once so neither side stalls on flow control.
    let sent: Vec<u8> = (0..3 << 20).map(|i: u32| (i % 251) as u8).collect();
    let (mut r, mut w) = tokio::io::split(stream);
    let to_send = sent.clone();
    let writer = tokio::spawn(async move {
        w.write_all(&to_send).await.unwrap();
        // Half-close: the echo answers what it has and then closes its side.
        w.shutdown().await.unwrap();
    });
    let mut back = Vec::new();
    r.read_to_end(&mut back).await.unwrap();
    writer.await.unwrap();
    assert_eq!(back.len(), sent.len());
    assert!(back == sent, "the echoed bytes differ");
    drop(r);
    stream = link.open_forward(port).await.unwrap();
    stream.write_all(b"again").await.unwrap();
    let mut buf = [0u8; 5];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"again");
}

#[tokio::test]
async fn a_port_nothing_listens_on_and_the_agents_own_ports_are_refused_with_a_reason() {
    let rig = rig("refused").await;
    let link = rig.link().await;
    // Bound but never listening: the port stays ours, so no parallel test can listen on it, and
    // a connection to it is refused.
    let unlistening = tokio::net::TcpSocket::new_v4().unwrap();
    unlistening.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let closed = unlistening.local_addr().unwrap().port();
    match link.open_forward(closed).await {
        Err(OpenError::Refused(reason)) => {
            assert!(reason.contains(&format!("port {closed}")), "{reason}");
        }
        other => panic!("{other:?}"),
    }
    // The proxy the agent serves on its own port: listening, but not for the host's forwards.
    let proxy = rig.agent.local_addr().port();
    match link.open_forward(proxy).await {
        Err(OpenError::Refused(reason)) => assert!(reason.contains("agent"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_guest_server_that_resets_the_connection_resets_the_forward() {
    let rig = rig("reset").await;
    let link = rig.link().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        // Wait for the first byte, so the agent's connect has surely finished.
        conn.read_exact(&mut [0u8; 1]).await.unwrap();
        conn.set_zero_linger().unwrap();
        drop(conn);
    });
    let mut stream = link.open_forward(port).await.unwrap();
    stream.write_all(b"x").await.unwrap();
    let mut buf = Vec::new();
    let end = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut buf))
        .await
        .expect("the reset did not reach the host");
    // The guest's reset arrives as a reset, never as a clean end: a cut-off reply must not look
    // complete.
    assert!(end.is_err(), "a reset ended as a clean close: {end:?}");
    assert_eq!(buf, Vec::<u8>::new());
}

#[tokio::test]
async fn the_agent_reports_the_listening_ports_at_start_and_on_every_change() {
    let rig = rig("ports").await;
    rig.listening(&[]);
    until(|| rig.told.ports.lock().unwrap().last().cloned()).await;
    rig.listening(&[8080, 3000]);
    until(|| (rig.told.ports.lock().unwrap().last() == Some(&vec![3000, 8080])).then_some(()))
        .await;
    // The agent's own proxy port is never listed, even when the table has it.
    let own = rig.agent.local_addr().port();
    rig.listening(&[own, 5000]);
    until(|| (rig.told.ports.lock().unwrap().last() == Some(&vec![5000])).then_some(())).await;
}
