// SPDX-License-Identifier: GPL-3.0-or-later
//! A proxy with two routes (one attacked, one bystander) and the guest side that sends them bytes.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use puddle_agent_proto::tokio_yamux::{Control, Session, StreamHandle};
use puddle_agent_proto::yamux::client_config;
use puddle_ipc::IpcRoot;
use puddle_proxy::testing::{AnyAddress, StaticPolicy, StaticResolver};
use puddle_proxy::{Proxy, Route};
use puddle_types::{Host, NullSink, WorkspaceName};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// A proxy with two routes, `attacked` and `bystander`, one policy and an echo server on loopback.
pub(crate) struct Rig {
    pub(crate) policy: Arc<StaticPolicy>,
    pub(crate) attacked: Route,
    pub(crate) bystander: Route,
    pub(crate) echo: SocketAddr,
    _root: IpcRoot,
}

pub(crate) async fn rig() -> Rig {
    let policy = Arc::new(StaticPolicy::new());
    policy.allow(&Host::parse_normalised("127.0.0.1").unwrap());
    let proxy = Arc::new(
        Proxy::new(policy.clone(), Arc::new(NullSink))
            .with_resolver(Arc::new(StaticResolver::new()))
            .with_address_check(Arc::new(AnyAddress)),
    );
    let root = IpcRoot::new().unwrap();
    let attacked = proxy.serve_route(
        root.listen().unwrap(),
        WorkspaceName::new("attacked").unwrap(),
    );
    let bystander = proxy.serve_route(
        root.listen().unwrap(),
        WorkspaceName::new("bystander").unwrap(),
    );
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let echo = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    Rig {
        policy,
        attacked,
        bystander,
        echo,
        _root: root,
    }
}

pub(crate) struct Guest {
    pub(crate) control: Control,
    driver: JoinHandle<()>,
}

impl Drop for Guest {
    fn drop(&mut self) {
        self.driver.abort();
    }
}

impl Guest {
    pub(crate) async fn connect(route: &Route) -> Self {
        let conn = puddle_ipc::connect(route.endpoint().path()).await.unwrap();
        let mut session = Session::new_client(conn, client_config());
        let control = session.control();
        let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });
        Self { control, driver }
    }

    /// Sends `bytes` (a writer task, so a refusal that comes before the guest has finished
    /// writing is still read) and returns the status code (0 when no status line came back) and the response body.
    pub(crate) async fn refused(&mut self, bytes: Arc<[u8]>) -> (u16, String) {
        let stream: StreamHandle = self.control.open_stream().await.unwrap();
        let (mut read, mut write) = tokio::io::split(stream);
        let writer = tokio::spawn(async move {
            for chunk in bytes.chunks(64 * 1024) {
                if write.write_all(chunk).await.is_err() {
                    return;
                }
            }
            let _ = write.shutdown().await;
        });
        let mut raw = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            let mut chunk = [0_u8; 4096];
            loop {
                match read.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        raw.extend_from_slice(chunk.get(..n).unwrap_or_default());
                        if let Some(at) = find(&raw, b"\r\n\r\n") {
                            let want = content_length(raw.get(..at).unwrap_or_default());
                            if raw.len() >= at + 4 + want {
                                break;
                            }
                        }
                    }
                }
            }
        })
        .await;
        writer.abort();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let code = text
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned();
        (code, body)
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn content_length(head: &[u8]) -> usize {
    String::from_utf8_lossy(head)
        .lines()
        .find_map(|l| {
            l.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .map(|v| v.trim().parse().unwrap())
        })
        .unwrap_or(0)
}

/// 64 CONNECT requests through the bystander's route, each with an echo round trip; returns how long.
pub(crate) async fn bystander_round(route: &Route, echo: SocketAddr) -> Duration {
    let started = Instant::now();
    let mut guest = Guest::connect(route).await;
    for n in 0..64_u32 {
        let mut stream = guest.control.open_stream().await.unwrap();
        stream
            .write_all(format!("CONNECT {echo} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            head.push(reader.read_u8().await.unwrap());
        }
        assert!(
            head.starts_with(b"HTTP/1.1 200"),
            "{}",
            String::from_utf8_lossy(&head)
        );
        let ping = n.to_be_bytes();
        reader.get_mut().write_all(&ping).await.unwrap();
        let mut echoed = [0_u8; 4];
        reader.read_exact(&mut echoed).await.unwrap();
        assert_eq!(echoed, ping);
    }
    started.elapsed()
}
