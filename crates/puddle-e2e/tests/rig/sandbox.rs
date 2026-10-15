// SPDX-License-Identifier: GPL-3.0-or-later
//! The sandbox side of the corporate-lab tests: the real guest agent, the real proxy (SQLite
//! rules and audit) and upstream chaining over a chosen OS proxy layer, plus the small clients
//! and servers around them. The fixtures themselves are `puddle_e2e::lab`.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]
#![allow(dead_code, reason = "each test binary uses some of the pieces")]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::LOCAL;
use puddle_agent::config::Target;
use puddle_agent::{Agent, Config};
use puddle_ipc::IpcRoot;
use puddle_proxy::testing::{AnyAddress, StaticResolver};
use puddle_proxy::{Proxy, Route, Upstream};
use puddle_store::{Actor, Effect, Limits, NewRule, Pattern, Scope, Store, SystemClock};
use puddle_types::{NullSink, WorkspaceName};
use puddle_upstream::{
    BasicAuth, Chain, Config as DiscoveryConfig, Credentials, Discovery, FakeOs, Hop, OsProxy,
    ProxyAddr, ProxyAuth, ProxyConfig,
};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub(crate) fn workspace() -> WorkspaceName {
    WorkspaceName::new("e2e").unwrap()
}

pub(crate) struct Rig {
    pub(crate) store: Arc<Store>,
    pub(crate) agent: Agent,
    _route: Route,
    _root: IpcRoot,
}

/// The proxy over `hops` (a PAC answer), authenticating with `auth`; names are unknown to the
/// resolver, so only Squid can reach them.
pub(crate) async fn rig(hops: Vec<Hop>, auth: Arc<dyn ProxyAuth>) -> Rig {
    let os = FakeOs::new(ProxyConfig {
        pac_url: Some("http://pac.corp/proxy.pac".into()),
        ..ProxyConfig::default()
    });
    os.set_pac(move |_| Ok(hops.clone()));
    rig_over(os, auth).await
}

/// The proxy over any OS proxy layer (a real PAC engine, say), authenticating with `auth`.
pub(crate) async fn rig_over(os: Arc<dyn OsProxy>, auth: Arc<dyn ProxyAuth>) -> Rig {
    let store = Arc::new(Store::open_in_memory(Arc::new(SystemClock), Limits::default()).unwrap());
    let chain = Chain::new(Discovery::new(os, DiscoveryConfig::default()), auth);
    let proxy = Arc::new(
        Proxy::new(store.clone(), Arc::new(NullSink))
            .with_connection_log(store.clone())
            .with_resolver(Arc::new(StaticResolver::new()))
            .with_address_check(Arc::new(AnyAddress))
            .with_upstream(Upstream::new(chain)),
    );
    let root = IpcRoot::new().unwrap();
    let route = proxy.serve_route(root.listen().unwrap(), workspace());
    let config = Config {
        listen: SocketAddr::new(IpAddr::V4(LOCAL), 0),
        target: Target::Unix(route.endpoint().path().to_path_buf()),
        oom: None,
        bridge: None,
        ..Config::default()
    };
    let agent = Agent::start(config).await.unwrap();
    Rig {
        store,
        agent,
        _route: route,
        _root: root,
    }
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

pub(crate) async fn connection_records(store: &Store, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let records: Vec<Value> = store
            .audit_lines(0, 100_000)
            .unwrap()
            .into_iter()
            .map(|(_, line)| serde_json::from_str::<Value>(&line).unwrap())
            .filter(|v| v["type"] == "connection")
            .collect();
        if records.len() >= count || Instant::now() >= deadline {
            assert!(
                records.len() >= count,
                "{count} records expected: {records:?}"
            );
            return records;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

pub(crate) async fn status_and_body(
    agent: SocketAddr,
    request: &str,
) -> (u16, Vec<String>, String) {
    let mut conn = TcpStream::connect(agent).await.unwrap();
    conn.write_all(request.as_bytes()).await.unwrap();
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let code: u16 = line.split(' ').nth(1).unwrap().parse().unwrap();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).await.unwrap();
        if h == "\r\n" || h.is_empty() {
            break;
        }
        headers.push(h.trim_end().to_ascii_lowercase());
    }
    let mut body = String::new();
    let _ = reader.read_to_string(&mut body).await;
    (code, headers, body)
}

pub(crate) fn basic(user: &str, password: &str) -> Arc<dyn ProxyAuth> {
    Arc::new(BasicAuth::new().with_default(Credentials::new(user, password)))
}

/// A proxy address nothing answers on: bound but never listening, so the port stays ours and
/// connecting is refused.
pub(crate) fn dead_proxy() -> (ProxyAddr, tokio::net::TcpSocket) {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind(SocketAddr::new(IpAddr::V4(LOCAL), 0)).unwrap();
    let port = socket.local_addr().unwrap().port();
    (ProxyAddr::new("127.0.0.1", port), socket)
}

/// Opens a `CONNECT authority` tunnel through the agent; the stream after the `200` headers.
pub(crate) async fn tunnel(agent: SocketAddr, authority: &str) -> BufReader<TcpStream> {
    try_tunnel(agent, authority).await.unwrap()
}

/// Like [`tunnel`], with the status line as the error, so a test can add its fixture's log.
pub(crate) async fn try_tunnel(
    agent: SocketAddr,
    authority: &str,
) -> Result<BufReader<TcpStream>, String> {
    let mut conn = TcpStream::connect(agent).await.unwrap();
    conn.write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut reader = BufReader::new(conn);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    if !line.starts_with("HTTP/1.1 200") {
        return Err(line);
    }
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).await.unwrap();
        if h == "\r\n" {
            break;
        }
    }
    Ok(reader)
}

/// Sends 64 KiB through an established tunnel to an echo server and returns what came back.
pub(crate) async fn echo_roundtrip(reader: BufReader<TcpStream>) -> (Vec<u8>, Vec<u8>) {
    let payload: Vec<u8> = (0..64 * 1024usize)
        .map(|n| u8::try_from(n % 251).unwrap())
        .collect();
    // No half-close: a proxy may end the whole tunnel on the client's FIN, so read the echo in full.
    let (mut rd, mut wr) = tokio::io::split(reader);
    let upload = async {
        wr.write_all(&payload).await.unwrap();
    };
    let download = async {
        let mut back = vec![0_u8; payload.len()];
        rd.read_exact(&mut back).await.unwrap();
        back
    };
    let ((), back) = tokio::join!(upload, download);
    (payload, back)
}
