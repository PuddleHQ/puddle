// SPDX-License-Identifier: GPL-3.0-or-later
//! A proxy with two routes (one attacked, one bystander) and the guest side that sends them bytes.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses a different subset")]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use puddle_agent_proto::tokio_yamux::{Control, Session, StreamHandle};
use puddle_agent_proto::yamux::client_config;
use puddle_ipc::IpcRoot;
use puddle_proxy::testing::{AnyAddress, StaticPolicy, StaticResolver};
use puddle_proxy::{Proxy, ProxyConfig, Route};
use puddle_test_kit::{EventSource, next_event_within};
use puddle_types::{ConnectionLog, Host, NullConnectionLog, NullSink, WorkspaceName};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::TcpListener;
use tokio::process::{Child, ChildStderr, ChildStdout, Command};
use tokio::task::JoinHandle;

const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Set in the child process to the attacked route's endpoint: the child is then the guests.
const GUEST_ROUTE: &str = "PUDDLE_TEST_HOSTILE_GUEST_ROUTE";

/// The attacked route's endpoint when this process is a test's guests (see [`Guests`]).
pub(crate) fn guest_route() -> Option<PathBuf> {
    std::env::var_os(GUEST_ROUTE).map(PathBuf::from)
}

/// The child's stdout a line at a time, as the events [`next_event_within`] waits for.
struct GuestLines(Lines<BufReader<ChildStdout>>);

impl EventSource for GuestLines {
    type Event = String;
    async fn next(&mut self) -> Option<String> {
        self.0.next_line().await.unwrap()
    }
}

/// A test's guests in a child process: the same test binary, re-run as `test` with
/// [`guest_route`] set. A real guest runs in its own machine, so what a guest holds (its streams,
/// the bytes it has queued but not yet sent) is its memory, not the proxy's, and the heap a test
/// counts in the parent is the proxy's.
pub(crate) struct Guests {
    child: Child,
    lines: GuestLines,
    stderr: ChildStderr,
    said: String,
}

impl Guests {
    pub(crate) fn start(test: &str, route: &Route) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(GUEST_ROUTE, route.endpoint().path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        Self {
            lines: GuestLines(BufReader::new(child.stdout.take().unwrap()).lines()),
            stderr: child.stderr.take().unwrap(),
            child,
            said: String::new(),
        }
    }

    /// Reads what the guests print until they print `line`. The bound is progress, not wall time: the
    /// guests' work takes CPU time, so on a starved machine it takes as long as it takes. Each line they
    /// print restarts the `stall` window; the panic names `line`, the last thing the guests said and
    /// what they said in all when `stall` passes with nothing new (or when they end first).
    pub(crate) async fn until(&mut self, line: &str, stall: Duration) {
        let mut last = String::from("nothing yet");
        while !self.said.contains(line) {
            let what = format!(
                "the guests, waiting for {line:?}, printed nothing new (last said {last:?}; all: {:?})",
                self.said
            );
            let said = next_event_within(&what, stall, &mut self.lines).await;
            self.said.push_str(&said);
            self.said.push('\n');
            last = said;
        }
    }

    /// Waits up to `limit` for the guests to end: whether they succeeded, and all they printed.
    pub(crate) async fn finish(mut self, limit: Duration) -> (bool, String) {
        // The rest goes into its own strings: a read cut off by the timeout drops what it was filling.
        let (mut rest, mut stderr) = (String::new(), String::new());
        let status = tokio::time::timeout(limit, async {
            let (out, err) = tokio::join!(
                self.lines.0.get_mut().read_to_string(&mut rest),
                self.stderr.read_to_string(&mut stderr)
            );
            out.unwrap();
            err.unwrap();
            self.child.wait().await.unwrap()
        })
        .await;
        let Ok(status) = status else {
            return (
                false,
                format!(
                    "the guests did not finish within {limit:?}; they said: {}",
                    self.said
                ),
            );
        };
        let said = format!(
            "exit status {status}; stdout: {:?}{rest:?}; stderr: {stderr:?}",
            self.said
        );
        (status.success(), said)
    }
}

/// A proxy with two routes, `attacked` and `bystander`, one policy and an echo server on loopback.
pub(crate) struct Rig {
    pub(crate) policy: Arc<StaticPolicy>,
    pub(crate) attacked: Route,
    pub(crate) bystander: Route,
    pub(crate) echo: SocketAddr,
    _root: IpcRoot,
}

pub(crate) async fn rig() -> Rig {
    rig_with(&ProxyConfig::default()).await
}

/// As [`rig`], with `config` for the proxy.
pub(crate) async fn rig_with(config: &ProxyConfig) -> Rig {
    rig_logged(config, Arc::new(NullConnectionLog)).await
}

/// As [`rig_with`], with `log` as the proxy's connection log.
pub(crate) async fn rig_logged(config: &ProxyConfig, log: Arc<dyn ConnectionLog>) -> Rig {
    let policy = Arc::new(StaticPolicy::new());
    policy.allow(&Host::parse_normalised("127.0.0.1").unwrap());
    let proxy = Arc::new(
        Proxy::new(policy.clone(), Arc::new(NullSink))
            .with_resolver(Arc::new(StaticResolver::new()))
            .with_address_check(Arc::new(AnyAddress))
            .with_connection_log(log)
            .with_config(*config),
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
        Self::connect_at(route.endpoint().path()).await
    }

    /// A guest on the route whose endpoint is `path` (from another process, which has no [`Route`]).
    pub(crate) async fn connect_at(path: &Path) -> Self {
        let conn = puddle_ipc::connect(path).await.unwrap();
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

impl Guest {
    /// Opens one stream over the workspace's cap: `true` when its answer is the cap's refusal
    /// naming `limit`, `None` when it got no answer within 20 s.
    pub(crate) async fn refused_at_the_cap(&self, limit: usize) -> Option<bool> {
        let mut control = self.control.clone();
        let mut stream = control.open_stream().await.ok()?;
        stream
            .write_all(b"CONNECT blocked.test:443 HTTP/1.1\r\n\r\n")
            .await
            .ok()?;
        let mut raw = Vec::new();
        tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut raw))
            .await
            .ok()?
            .ok()?;
        let text = String::from_utf8_lossy(&raw);
        Some(
            text.starts_with("HTTP/1.1 503")
                && text.contains(&format!(
                    "too many open connections from this workspace (limit {limit})"
                )),
        )
    }

    /// Opens `CONNECT echo` and returns the status code and the stream (open, for the caller to hold).
    pub(crate) async fn connect_status(&mut self, echo: SocketAddr) -> (u16, StreamHandle) {
        let mut stream = self.control.open_stream().await.unwrap();
        stream
            .write_all(format!("CONNECT {echo} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            match stream.read_u8().await {
                Ok(byte) => head.push(byte),
                Err(_) => break,
            }
        }
        let code = String::from_utf8_lossy(&head)
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        (code, stream)
    }
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
