// SPDX-License-Identifier: GPL-3.0-or-later
//! Shared harness: a host over a Unix socket (yamux server, control stream, allow-all CONNECT
//! endpoint) and an agent pointed at it.
#![expect(
    clippy::unwrap_used,
    reason = "the shared harness runs only in tests, where a panic is how a test fails"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use puddle_agent::config::{OomSources, Target};
use puddle_agent::{Agent, Config};
use puddle_agent_proto::host::{HostConfig, serve_session};
use puddle_agent_proto::testing::AllowAll;
use puddle_types::{Event, EventSink, WorkspaceName};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// A loopback port that stays this test's until the value is dropped, for a config that must name
/// its port before anything listens on it (the editor server's `--port`). The socket is bound and
/// never listens, so connecting to the port is refused, as on a free one, while another process's
/// port 0 or plain bind cannot take it. `SO_REUSEADDR` lets the test's own listener (the server
/// that "answers") bind the same number and be dropped again, as many times as the test needs.
pub struct ReservedPort {
    _socket: tokio::net::TcpSocket,
    port: u16,
}

impl ReservedPort {
    pub fn new() -> Self {
        let socket = tokio::net::TcpSocket::new_v4().unwrap();
        socket.set_reuseaddr(true).unwrap();
        socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let port = socket.local_addr().unwrap().port();
        Self {
            _socket: socket,
            port,
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

pub struct ChanSink(pub mpsc::UnboundedSender<Event>);

impl EventSink for ChanSink {
    fn emit(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

/// A per-test temp dir, removed on drop.
pub struct TempDir(pub PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "pa-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn workspace() -> WorkspaceName {
    WorkspaceName::new("agent").unwrap()
}

pub struct Rig {
    pub agent: Agent,
    pub events: mpsc::UnboundedReceiver<Event>,
    pub sink: Arc<dyn EventSink>,
    pub host: JoinHandle<()>,
    pub dir: TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.host.abort();
    }
}

impl Rig {
    pub fn socket(&self) -> PathBuf {
        self.dir.0.join("route.sock")
    }

    /// Stops the host (every session to the agent dies) and starts a fresh one on the same path.
    pub async fn restart_host(&mut self) {
        self.host.abort();
        let _ = (&mut self.host).await;
        std::fs::remove_file(self.socket()).unwrap();
        self.host = start_host(&self.socket(), Arc::clone(&self.sink));
    }
}

/// Raises this process's open-file soft limit (to 8192, or the hard limit if lower). A test runs
/// every party of its connections here (client, agent, host side, server: about four descriptors
/// each), so 256 at once run out under the 1024 a process often starts with.
fn enough_open_files() {
    use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
    const WANT: u64 = 8192;
    let limit = getrlimit(Resource::Nofile);
    let want = limit.maximum.map_or(WANT, |max| max.min(WANT));
    if limit.current.is_some_and(|current| current < want) {
        setrlimit(
            Resource::Nofile,
            Rlimit {
                current: Some(want),
                maximum: limit.maximum,
            },
        )
        .unwrap();
    }
}

pub fn start_host(socket: &Path, sink: Arc<dyn EventSink>) -> JoinHandle<()> {
    enough_open_files();
    let listener = UnixListener::bind(socket).unwrap();
    tokio::spawn(async move {
        let mut sessions = tokio::task::JoinSet::new();
        loop {
            let (conn, _) = listener.accept().await.unwrap();
            let sink = Arc::clone(&sink);
            sessions.spawn(async move {
                // A session ends with an error when the agent side goes away mid-frame; fine here.
                let _ = serve_session(
                    conn,
                    workspace(),
                    sink,
                    Arc::new(AllowAll),
                    HostConfig::default(),
                )
                .await;
            });
        }
    })
}

/// Starts a host on a Unix socket and an agent on 127.0.0.1:0 pointed at it.
/// `oom` gets the rig's temp dir and returns the OOM watch sources (or `None` for no watch).
pub async fn rig(tag: &str, oom: impl FnOnce(&Path) -> Option<OomSources>) -> Rig {
    rig_with(tag, oom, |_| {}, Arc::new(NoBridge)).await
}

/// A probe that never finds a bridge.
pub struct NoBridge;

impl puddle_agent::bridge::Probe for NoBridge {
    fn find(&self, _: std::net::Ipv4Addr) -> Option<u32> {
        None
    }
}

/// [`rig`] with a say in the config and the bridge probe.
pub async fn rig_with(
    tag: &str,
    oom: impl FnOnce(&Path) -> Option<OomSources>,
    tweak: impl FnOnce(&mut Config),
    probe: Arc<dyn puddle_agent::bridge::Probe>,
) -> Rig {
    let dir = TempDir::new(tag);
    let oom = oom(&dir.0);
    let socket = dir.0.join("route.sock");
    let (tx, events) = mpsc::unbounded_channel();
    let sink: Arc<dyn EventSink> = Arc::new(ChanSink(tx));
    let host = start_host(&socket, Arc::clone(&sink));
    let mut config = Config {
        listen: "127.0.0.1:0".parse().unwrap(),
        target: Target::Unix(socket),
        oom,
        ..Config::default()
    };
    tweak(&mut config);
    let agent = Agent::start_with_probe(config, probe).await.unwrap();
    Rig {
        agent,
        events,
        sink,
        host,
        dir,
    }
}

pub async fn next_event(
    rx: &mut mpsc::UnboundedReceiver<Event>,
    within: Duration,
) -> Option<Event> {
    tokio::time::timeout(within, rx.recv()).await.ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::ReservedPort;

    #[tokio::test]
    async fn a_reserved_port_refuses_connections_and_other_binds_but_takes_a_reusing_listener() {
        let reserved = ReservedPort::new();
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], reserved.port()));
        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "nothing listens on a reserved port"
        );
        // Another process picking by number, without SO_REUSEADDR, is turned away: the collision
        // a dropped listener leaves open.
        let squatter = tokio::net::TcpSocket::new_v4().unwrap();
        assert!(squatter.bind(addr).is_err(), "a plain bind took the port");
        // The test's own stand-in server binds it, answers, and goes away again.
        let listener = std::net::TcpListener::bind(addr).unwrap();
        assert!(tokio::net::TcpStream::connect(addr).await.is_ok());
        drop(listener);
        assert!(std::net::TcpListener::bind(addr).is_ok());
    }
}
