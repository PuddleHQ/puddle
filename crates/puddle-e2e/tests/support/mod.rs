// SPDX-License-Identifier: GPL-3.0-or-later
//! The rig the L2 tests share: the real guest agent, the real proxy and the real rules engine,
//! joined as in a workspace, with local servers to talk to.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses some of the helpers")]

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use puddle_agent::config::Target;
use puddle_agent::{Agent, Config};
use puddle_ipc::IpcRoot;
use puddle_proxy::testing::{AnyAddress, StaticResolver};
use puddle_proxy::{Proxy, Route};
use puddle_store::{Actor, Effect, Limits, NewRule, Pattern, Scope, Store, SystemClock};
use puddle_test_kit::eventually;
use puddle_types::{NullSink, WorkspaceName};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

pub(crate) mod stall;

pub(crate) const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

pub(crate) fn workspace() -> WorkspaceName {
    WorkspaceName::new("e2e").unwrap()
}

pub(crate) struct Rig {
    pub(crate) store: Arc<Store>,
    pub(crate) agent: Agent,
    pub(crate) route: Route,
    _root: IpcRoot,
}

pub(crate) async fn rig() -> Rig {
    // Four descriptors per proxied connection (client, agent, proxy's upstream, echo server): the
    // 256-connection test alone needs about 1 000, which is the usual default limit of 1 024.
    let _ = puddle_fd_limit::raise_open_file_limit(puddle_fd_limit::Reach::Soft);
    let store = Arc::new(Store::open_in_memory(Arc::new(SystemClock), Limits::default()).unwrap());
    let resolver = StaticResolver::new()
        .with("echo.test", &[LOCAL])
        .with("sink.test", &[LOCAL])
        .with("new.test", &[LOCAL]);
    let proxy = Arc::new(
        Proxy::new(store.clone(), Arc::new(NullSink))
            .with_connection_log(store.clone())
            .with_resolver(Arc::new(resolver))
            .with_address_check(Arc::new(AnyAddress)),
    );
    let root = IpcRoot::new().unwrap();
    let route = proxy.serve_route(root.listen().unwrap(), workspace());
    let config = Config {
        listen: SocketAddr::new(LOCAL, 0),
        target: Target::Unix(route.endpoint().path().to_path_buf()),
        oom: None,
        // Never the machine's real docker0.
        bridge: None,
        ..Config::default()
    };
    let agent = Agent::start(config).await.unwrap();
    Rig {
        store,
        agent,
        route,
        _root: root,
    }
}

/// Prints the proxy's own log lines (info and up) to stderr, which nextest shows only for a test
/// that fails. A guest is told "502" and nothing more when the proxy cannot connect out; the line
/// with the operating system's error is the only record of why. For the load tests, whose failures
/// have no other trace.
pub(crate) fn log_proxy_to_stderr() {
    let _ = tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_writer(io::stderr)
            .with_ansi(false)
            .finish(),
    );
}

pub(crate) fn allow(store: &Store, host: &str) {
    store
        .add_rule(&NewRule {
            scope: Scope::Workspace(workspace()),
            pattern: Pattern::parse(host).unwrap(),
            effect: Effect::Allow,
            expires_at: None,
            created_by: Actor::Cli,
        })
        .unwrap();
}

/// The `connection` audit records so far, oldest first.
pub(crate) fn connection_records(store: &Store) -> Vec<Value> {
    store
        .audit_lines(0, 100_000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| serde_json::from_str::<Value>(&line).unwrap())
        .filter(|v| v["type"] == "connection")
        .collect()
}

/// Waits up to 5 s for at least `count` `connection` records (one is written when its
/// connection ends, after the guest has its answer).
pub(crate) async fn wait_for_records(store: &Store, count: usize) -> Vec<Value> {
    eventually(
        &format!("{count} connection records"),
        Duration::from_secs(5),
        || {
            let records = connection_records(store);
            if records.len() >= count {
                Ok(records)
            } else {
                Err(format!("{records:?}"))
            }
        },
    )
    .await
}

/// An echo server: sends back everything, then closes after the client's FIN.
pub(crate) fn echo_server() -> SocketAddr {
    // The agent's backlog: 2 000 proxied connections dial this at once, and tokio's default of
    // 128 would overflow the accept queue.
    let listener = puddle_agent::serve::listen(SocketAddr::new(LOCAL, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let (mut r, mut w) = conn.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
                let _ = w.shutdown().await;
            });
        }
    });
    addr
}

/// Sends `CONNECT authority` through the agent; returns the status code and the connection.
pub(crate) async fn connect_via(
    agent: SocketAddr,
    authority: &str,
) -> io::Result<(u16, BufReader<TcpStream>)> {
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
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).await?;
        if h == "\r\n" || h.is_empty() {
            return Ok((code, reader));
        }
    }
}

/// The 64 KiB connection `i` sends: byte `n` is `(n + i) % 251`. Built once, outside the timed
/// part: in a coverage build, making it per connection cost more than carrying it through the
/// agent and the proxy, so a thousand connections at once timed the test instead of Puddle.
fn echo_payload(i: usize) -> &'static [u8] {
    const LEN: usize = 64 * 1024;
    static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
    let bytes = BYTES.get_or_init(|| {
        (0..LEN + 251)
            .map(|n| u8::try_from(n % 251).unwrap())
            .collect()
    });
    bytes.split_at(i % 251).1.split_at(LEN).0
}

pub(crate) async fn round_trip(
    agent: SocketAddr,
    authority: &str,
    i: usize,
) -> io::Result<Duration> {
    let payload = echo_payload(i);
    let started = Instant::now();
    let (code, mut conn) = connect_via(agent, authority).await?;
    if code != 200 {
        return Err(io::Error::other(format!("connection {i}: status {code}")));
    }
    let (mut rd, mut wr) = tokio::io::split(&mut conn);
    let upload = async {
        wr.write_all(payload).await?;
        wr.shutdown().await
    };
    let mut back = Vec::new();
    let download = rd.read_to_end(&mut back);
    let (up, down) = tokio::join!(upload, download);
    up?;
    down?;
    if back != payload {
        return Err(io::Error::other(format!("connection {i}: echo mismatch")));
    }
    Ok(started.elapsed())
}
