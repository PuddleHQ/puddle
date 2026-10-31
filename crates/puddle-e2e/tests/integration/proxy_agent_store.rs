// SPDX-License-Identifier: GPL-3.0-or-later
//! L2 end to end, no VM: a guest TCP client → the real guest agent → yamux over the workspace's
//! real endpoint (`puddle-ipc`, a Unix socket) → the real proxy → local servers, with the real
//! SQLite rules engine (`puddle-store`) as the policy and as the proxy's connection log, so every
//! connection ends as a `connection` audit record (R-24).
//!
//! The only doubles are the resolver (`*.test` names point at loopback) and the address check
//! (loopback is where the test servers are).
#![cfg(unix)]
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use crate::support;

use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};

use puddle_agent::config::Target;
use puddle_agent::{Agent, Config};
use puddle_store::{Actor, Limits, PendingState, Resolution};
use puddle_test_kit::cpu_time;
use serde_json::Value;
use support::{
    LOCAL, allow, connect_via, connection_records, echo_server, rig, round_trip, wait_for_records,
    workspace,
};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

/// The load bar through the real agent and the real rules engine: 256 parallel `CONNECT`s,
/// 0 failures, none starved (slowest within 10 times the median plus 5 s).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_connects_256_through_agent_proxy_and_store() {
    support::log_proxy_to_stderr();
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let echo = echo_server();
    let authority = format!("echo.test:{}", echo.port());
    let agent = rig.agent.local_addr();
    let mut set = tokio::task::JoinSet::new();
    for i in 0..256 {
        let authority = authority.clone();
        set.spawn(async move { round_trip(agent, &authority, i).await });
    }
    let results = tokio::time::timeout(Duration::from_secs(60), async {
        let mut out = Vec::new();
        while let Some(r) = set.join_next().await {
            out.push(r.unwrap());
        }
        out
    })
    .await
    .expect("256 connections did not finish within 60 s");
    let failures: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(
        failures.is_empty(),
        "{} of 256 failed: {:?}",
        failures.len(),
        &failures[..failures.len().min(5)]
    );
    let mut took: Vec<Duration> = results
        .iter()
        .filter_map(|r| r.as_ref().ok().copied())
        .collect();
    took.sort();
    let (median, slowest) = (took[took.len() / 2], took[took.len() - 1]);
    // Relative, not a fixed bar: all 256 start together, so load stretches the median and the
    // slowest alike (1.8 s slowest on 4 cores under 16 busy loops); a connection starved while its
    // peers are served is what this catches.
    assert!(
        slowest < median * 10 + Duration::from_secs(5),
        "slowest connection took {slowest:?}, the median {median:?}"
    );
    assert_eq!(rig.store.open_pending(None).unwrap().len(), 0);

    // Every connection is in the audit: written, or counted in a `suppressed` summary once its
    // second is over (200 records per workspace per second, R-26).
    let deadline = Instant::now() + Duration::from_secs(10);
    let accounted = loop {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.store.sweep().unwrap();
        let records = connection_records(&rig.store);
        let written = records.iter().filter(|r| r["reason"] == "rule").count() as u64;
        let summarised: u64 = records.iter().filter_map(|r| r["count"].as_u64()).sum();
        if written + summarised >= 256 || Instant::now() >= deadline {
            break written + summarised;
        }
    };
    assert_eq!(accounted, 256);
}

/// The agent's listener holds a burst of connections nobody has accepted yet. Nothing accepts
/// here, so with tokio's default backlog of 128 the connections past the queue never finish
/// their handshake; with the agent's they all do. (On Linux the kernel caps the backlog at
/// `net.core.somaxconn`, 4 096 on current kernels.)
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_agent_listener_queues_a_burst_nobody_has_accepted() {
    const BURST: u64 = 1500;
    let somaxconn: u64 = std::fs::read_to_string("/proc/sys/net/core/somaxconn")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        somaxconn >= BURST + 100,
        "net.core.somaxconn is {somaxconn}: this machine cannot queue {BURST} connections"
    );
    let limit = puddle_fd_limit::raise_open_file_limit(puddle_fd_limit::Reach::Soft);
    assert!(
        limit.allows(BURST + 256),
        "the open-file limit cannot hold {BURST} sockets: {limit}"
    );
    let listener = puddle_agent::serve::listen(SocketAddr::new(LOCAL, 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..BURST {
        set.spawn(TcpStream::connect(addr));
    }
    let connected = tokio::time::timeout(Duration::from_secs(30), async {
        let mut open = Vec::new();
        while let Some(done) = set.join_next().await {
            open.push(done.unwrap().unwrap());
        }
        open.len()
    })
    .await
    .expect("connections past the accept queue never finished their handshake");
    assert_eq!(connected as u64, BURST);
}

/// The load bar at a real tool's worst case, with the process limit the product gives its own
/// processes: 2 000 `CONNECT`s through the real agent, proxy and rules engine, all open at the
/// same moment (each side of each connection holds a descriptor in this one process, 8 000 in
/// all), every one echoes a line back, none refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_thousand_connections_held_open_at_once_all_succeed() {
    const HELD: usize = 2000;
    let limit = puddle_fd_limit::raise_open_file_limit(puddle_fd_limit::Reach::Soft);
    eprintln!("{limit}");
    assert!(
        limit.allows(4 * HELD as u64 + 512),
        "this machine's hard open-file limit cannot hold {HELD} connections through one process: {limit}"
    );
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let echo = echo_server();
    let authority = format!("echo.test:{}", echo.port());
    let agent = rig.agent.local_addr();
    let all_open = Arc::new(tokio::sync::Barrier::new(HELD));
    let mut set = tokio::task::JoinSet::new();
    for i in 0..HELD {
        let authority = authority.clone();
        let all_open = Arc::clone(&all_open);
        set.spawn(async move {
            let opened = connect_via(agent, &authority).await;
            // Nobody sends before everybody has tried to connect: 2 000 are open at this point,
            // and a refused connection does not leave the others waiting.
            all_open.wait().await;
            let (code, mut conn) = opened?;
            if code != 200 {
                return Err(io::Error::other(format!("connection {i}: status {code}")));
            }
            conn.write_all(format!("line {i}\n").as_bytes()).await?;
            let mut line = String::new();
            conn.read_line(&mut line).await?;
            if line != format!("line {i}\n") {
                return Err(io::Error::other(format!("connection {i}: echoed {line:?}")));
            }
            Ok(())
        });
    }
    let results = tokio::time::timeout(Duration::from_secs(120), async {
        let mut out = Vec::new();
        while let Some(r) = set.join_next().await {
            out.push(r.unwrap());
        }
        out
    })
    .await
    .expect("2000 held connections did not finish within 120 s");
    let failures: Vec<_> = results.iter().filter_map(|r| r.as_ref().err()).collect();
    assert!(
        failures.is_empty(),
        "{} of {HELD} failed: {:?}",
        failures.len(),
        failures.first()
    );
}

/// R-24/R-25 end to end: the proxy's connection events land in the store's audit with decision,
/// rule or pending row, address and bytes, and no query string.
#[tokio::test]
async fn connections_land_in_the_store_audit() {
    const CANARY: &str = "CANARY-e2e-5b7d";
    let rig = rig().await;
    let echo = echo_server();
    let agent = rig.agent.local_addr();

    let pending = format!("new.test:{}", echo.port());
    let (code, _) = connect_via(agent, &pending).await.unwrap();
    assert_eq!(code, 403);
    let row = rig.store.open_pending(Some(&workspace())).unwrap()[0].id;
    wait_for_records(&rig.store, 1).await;

    let mut conn = TcpStream::connect(agent).await.unwrap();
    conn.write_all(
        format!(
            "GET http://{pending}/p/q?token={CANARY}#{CANARY} HTTP/1.1\r\nHost: new.test\r\n\r\n"
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut answer = String::new();
    conn.read_to_string(&mut answer).await.unwrap();
    assert!(answer.starts_with("HTTP/1.1 403"), "{answer}");
    wait_for_records(&rig.store, 2).await;

    allow(&rig.store, "echo.test");
    let authority = format!("echo.test:{}", echo.port());
    let (code, mut conn) = connect_via(agent, &authority).await.unwrap();
    assert_eq!(code, 200);
    conn.get_mut().write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    conn.read_exact(&mut back).await.unwrap();
    conn.get_mut().shutdown().await.unwrap();
    let mut rest = Vec::new();
    conn.read_to_end(&mut rest).await.unwrap();
    let records = wait_for_records(&rig.store, 3).await;

    let fields = |r: &Value| {
        (
            r["workspace_id"].clone(),
            r["host"].clone(),
            r["decision"].clone(),
            r["reason"].clone(),
            r["pending_id"].clone(),
        )
    };
    let pending_record = |r: &Value| {
        (
            Value::from("e2e"),
            Value::from("new.test"),
            Value::from("pending"),
            Value::from("no_rule"),
            Value::from(row.0),
        ) == fields(r)
    };
    assert!(pending_record(&records[0]), "{}", records[0]);
    assert!(pending_record(&records[1]), "{}", records[1]);
    assert!(records[0]["method"].is_null());
    assert_eq!(
        (&records[1]["method"], &records[1]["path"]),
        (&Value::from("GET"), &Value::from("/p/q"))
    );

    let allowed = &records[2];
    assert_eq!(
        (
            &allowed["host"],
            &allowed["decision"],
            &allowed["reason"],
            &allowed["resolved_ip"]
        ),
        (
            &Value::from("echo.test"),
            &Value::from("allow"),
            &Value::from("rule"),
            &Value::from("127.0.0.1")
        )
    );
    assert!(allowed["rule_id"].is_i64());
    let head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    let established = "HTTP/1.1 200 Connection Established\r\n\r\n";
    assert_eq!(allowed["bytes_up"], head.len() + 4);
    assert_eq!(allowed["bytes_down"], established.len() + 4);

    for (_, line) in rig.store.audit_lines(0, 1000).unwrap() {
        assert!(!line.contains(CANARY), "{line}");
    }
}

/// Deny ⇒ 403 and a pending row in the store; approving the row ⇒ the next attempt passes
/// without restarting anything (R-8, R-10, R-15).
#[tokio::test]
async fn an_unknown_host_is_pending_in_the_store_and_passes_after_approval() {
    let rig = rig().await;
    let echo = echo_server();
    let authority = format!("new.test:{}", echo.port());
    let agent = rig.agent.local_addr();

    let (code, mut conn) = connect_via(agent, &authority).await.unwrap();
    assert_eq!(code, 403);
    let mut body = String::new();
    conn.read_to_string(&mut body).await.unwrap();
    assert!(body.contains("waiting for approval in Puddle"), "{body}");

    let open = rig.store.open_pending(Some(&workspace())).unwrap();
    assert_eq!(open.len(), 1);
    let row = &open[0];
    assert_eq!(
        (row.host.to_string(), row.port, row.state),
        ("new.test".to_owned(), echo.port(), PendingState::Requested)
    );

    // A second attempt before approval is a repeat of the same row (R-11).
    let (code, _) = connect_via(agent, &authority).await.unwrap();
    assert_eq!(code, 403);
    assert_eq!(rig.store.pending(row.id).unwrap().attempts, 2);

    rig.store
        .resolve_pending(row.id, &Resolution::allow(), Actor::Cli)
        .unwrap();
    let (code, mut conn) = connect_via(agent, &authority).await.unwrap();
    assert_eq!(code, 200);
    conn.get_mut().write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    conn.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");
    assert_eq!(rig.store.open_pending(None).unwrap().len(), 0);
}

/// How a server-side connection ended.
#[derive(Debug, PartialEq, Eq)]
enum End {
    Eof(usize),
    Reset(usize),
    Other(String),
}

/// A sink server: reads to the end and reports how it ended; signals after the first bytes.
async fn sink_server() -> (
    SocketAddr,
    mpsc::UnboundedReceiver<End>,
    mpsc::UnboundedReceiver<()>,
) {
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (end_tx, ends) = mpsc::unbounded_channel();
    let (first_tx, firsts) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            let (mut conn, _) = listener.accept().await.unwrap();
            let (end_tx, first_tx) = (end_tx.clone(), first_tx.clone());
            tokio::spawn(async move {
                let mut total = 0;
                let mut buf = vec![0u8; 16 * 1024];
                let end = loop {
                    match conn.read(&mut buf).await {
                        Ok(0) => break End::Eof(total),
                        Ok(n) => {
                            if total == 0 {
                                let _ = first_tx.send(());
                            }
                            total += n;
                        }
                        Err(e) if e.kind() == io::ErrorKind::ConnectionReset => {
                            break End::Reset(total);
                        }
                        Err(e) => break End::Other(e.to_string()),
                    }
                };
                let _ = end_tx.send(end);
            });
        }
    });
    (addr, ends, firsts)
}

/// Through the product proxy: an aborted upload reaches the server as a reset, not as a
/// complete-looking EOF.
#[tokio::test]
async fn an_aborted_guest_upload_reaches_the_server_as_a_reset() {
    let rig = rig().await;
    allow(&rig.store, "sink.test");
    let (sink, mut ends, mut firsts) = sink_server().await;
    let authority = format!("sink.test:{}", sink.port());
    let (code, mut conn) = connect_via(rig.agent.local_addr(), &authority)
        .await
        .unwrap();
    assert_eq!(code, 200);
    conn.get_mut()
        .write_all(&vec![7u8; 32 * 1024])
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), firsts.recv())
        .await
        .unwrap();
    conn.get_ref().set_zero_linger().unwrap();
    drop(conn);
    let end = tokio::time::timeout(Duration::from_secs(5), ends.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(end, End::Reset(_)),
        "server saw {end:?}, not a reset"
    );
}

#[tokio::test]
async fn a_graceful_upload_reaches_the_server_complete() {
    let rig = rig().await;
    allow(&rig.store, "sink.test");
    let (sink, mut ends, _firsts) = sink_server().await;
    let authority = format!("sink.test:{}", sink.port());
    let (code, mut conn) = connect_via(rig.agent.local_addr(), &authority)
        .await
        .unwrap();
    assert_eq!(code, 200);
    conn.get_mut().write_all(&vec![7u8; 1 << 20]).await.unwrap();
    conn.get_mut().shutdown().await.unwrap();
    let end = tokio::time::timeout(Duration::from_secs(10), ends.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, End::Eof(1 << 20));
}

#[tokio::test]
async fn a_server_reset_reaches_the_guest_client_as_a_reset() {
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let listener = TcpListener::bind((LOCAL, 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (go_tx, go_rx) = oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut conn, _) = listener.accept().await.unwrap();
        conn.write_all(b"partial").await.unwrap();
        let _ = go_rx.await;
        conn.set_zero_linger().unwrap();
        drop(conn);
    });
    let (code, mut conn) = connect_via(rig.agent.local_addr(), &format!("echo.test:{port}"))
        .await
        .unwrap();
    assert_eq!(code, 200);
    let mut first = [0u8; 7];
    conn.read_exact(&mut first).await.unwrap();
    go_tx.send(()).unwrap();
    let mut rest = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut rest))
        .await
        .unwrap();
    assert_eq!(
        read.map_err(|e| e.kind()).err(),
        Some(io::ErrorKind::ConnectionReset),
        "the guest client must see the reset, not a clean end"
    );
}

/// A probe that always finds a bridge at index 1: the kernel's docker0, faked.
struct BridgeUp;

impl puddle_agent::bridge::Probe for BridgeUp {
    fn find(&self, _: Ipv4Addr) -> Option<u32> {
        Some(1)
    }
}

/// A request that arrives on the Docker bridge listener meets the same policy and lands
/// in the same audit as one on the loopback listener: same workspace, same pending row (the
/// repeat bumps `attempts`), same rule once approved, same record shape.
#[tokio::test]
async fn the_bridge_listener_gets_the_same_policy_and_audit_as_loopback() {
    let rig = rig().await;
    // A second agent on the same route, listening on the faked bridge address (127.0.0.2).
    let config = Config {
        listen: SocketAddr::new(LOCAL, 0),
        target: Target::Unix(rig.route.endpoint().path().to_path_buf()),
        oom: None,
        bridge: Some(puddle_agent::config::BridgeConfig {
            addr: std::net::SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 0),
            poll: Duration::from_millis(10),
        }),
        ..Config::default()
    };
    let agent = Agent::start_with_probe(config, Arc::new(BridgeUp))
        .await
        .unwrap();
    let mut state = agent.bridge_state();
    let bridge = tokio::time::timeout(
        Duration::from_secs(10),
        state.wait_for(|s| s.addr.is_some()),
    )
    .await
    .unwrap()
    .unwrap()
    .addr
    .unwrap();
    let echo = echo_server();
    let authority = format!("new.test:{}", echo.port());

    // Unknown host: refused and pending, on both listeners.
    let (code, _) = connect_via(rig.agent.local_addr(), &authority)
        .await
        .unwrap();
    assert_eq!(code, 403);
    let (code, _) = connect_via(bridge, &authority).await.unwrap();
    assert_eq!(code, 403);
    let pending = rig.store.open_pending(Some(&workspace())).unwrap();
    assert_eq!(pending.len(), 1, "one pending item for both listeners");
    assert_eq!(pending[0].attempts, 2);
    let records = wait_for_records(&rig.store, 2).await;
    let shape = |r: &Value| {
        (
            r["workspace_id"].clone(),
            r["host"].clone(),
            r["decision"].clone(),
            r["reason"].clone(),
            r["pending_id"].clone(),
        )
    };
    assert_eq!(shape(&records[0]), shape(&records[1]));

    // One approval covers both; the bridge then tunnels.
    allow(&rig.store, "new.test");
    let (code, mut conn) = connect_via(bridge, &authority).await.unwrap();
    assert_eq!(code, 200);
    conn.get_mut().write_all(b"ping").await.unwrap();
    let mut back = [0u8; 4];
    conn.read_exact(&mut back).await.unwrap();
    assert_eq!(&back, b"ping");
    drop(conn);
    let records = wait_for_records(&rig.store, 3).await;
    assert_eq!(records[2]["decision"], "allow");
    assert_eq!(records[2]["workspace_id"], "e2e");
}

/// Every SSH connection is refused at once with its own audit reason, whatever the host, the port
/// and the rules: it never becomes a pending row, never counts as an allow, and the host that a
/// rule allows is refused just the same. The client is the agent's `connect` command (what `ssh`
/// runs as its `ProxyCommand`) speaking an SSH identification line.
#[tokio::test]
async fn ssh_is_refused_never_pending_and_audited_with_its_own_reason() {
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let settings = puddle_agent::connect::Settings::from_config(&Config {
        listen: rig.agent.local_addr(),
        ..Config::default()
    });
    let destinations = [
        ("echo.test", 22),
        ("new.test", 2222),
        ("sink.test", 443),
        ("github.com", 22),
        ("ssh.github.com", 443),
        ("2001:db8::1", 22),
    ];
    for (host, port) in destinations {
        let request = puddle_agent::connect::Request::parse(&[host, &port.to_string()]).unwrap();
        let (mut ssh, stdin) = tokio::io::duplex(1024);
        let (stdout, mut printed) = tokio::io::duplex(1024);
        ssh.write_all(b"SSH-2.0-OpenSSH_10.0p2\r\n").await.unwrap();
        let mut stderr = Vec::new();
        let exit =
            puddle_agent::connect::run(&settings, &request, stdin, stdout, &mut stderr).await;
        assert_eq!(exit, puddle_agent::connect::Exit::Failed, "{host}:{port}");
        let mut out = Vec::new();
        printed.read_to_end(&mut out).await.unwrap_or_default();
        assert!(out.is_empty(), "nothing a client could take for a server");
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(
            stderr.starts_with("puddle: SSH is not supported yet"),
            "{host}:{port}: {stderr}"
        );
        assert_eq!(
            stderr.contains("use HTTPS"),
            host.ends_with("github.com"),
            "{host}"
        );
    }
    let records = wait_for_records(&rig.store, destinations.len()).await;
    assert_eq!(records.len(), destinations.len(), "{records:?}");
    // Each record is written when its own connection ends, after the client has its answer, so
    // two that follow each other can be stored in either order: find each by its destination.
    for (host, port) in destinations {
        let mut own = records
            .iter()
            .filter(|r| r["host"] == host && r["port"] == port);
        let record = own
            .next()
            .unwrap_or_else(|| panic!("{host}:{port}: {records:?}"));
        assert!(own.next().is_none(), "{host}:{port} twice: {records:?}");
        assert_eq!(record["decision"], "blocked", "{record}");
        assert_eq!(record["reason"], "ssh_unsupported", "{record}");
        assert!(record["pending_id"].is_null(), "{record}");
        assert!(record["resolved_ip"].is_null(), "never connected: {record}");
    }
    assert_eq!(
        rig.store.open_pending(None).unwrap().len(),
        0,
        "nothing in the inbox"
    );
    // No rule was made, and the one that allows echo.test still decides everything else.
    assert_eq!(rig.store.rules().len(), 1);
}

/// What the transparent listener sends for a captured raw SSH client (libssh2, Go): the `CONNECT`
/// with the client's identification line right behind it, with the header or without. The real proxy
/// refuses it before the rules, and it is audited as SSH, never as a pending row, even for a host
/// a rule allows and an address nobody has seen.
#[tokio::test]
async fn a_captured_raw_ssh_client_is_refused_before_the_rules_and_never_pending() {
    let rig = rig().await;
    allow(&rig.store, "echo.test");
    let config = Config {
        target: Target::Unix(rig.route.endpoint().path().to_path_buf()),
        ..Config::default()
    };
    let upstream = puddle_agent::upstream::Upstream::new(&config);
    let attempts = [
        ("echo.test:22", true),
        ("192.0.2.22:22", true),
        ("192.0.2.23:2222", false),
        ("new.test:22", false),
    ];
    for (authority, header) in attempts {
        let mut stream = upstream.open().await.unwrap();
        let header = if header {
            "x-puddle-protocol: ssh\r\n"
        } else {
            ""
        };
        let head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{header}\r\n");
        stream.write_all(head.as_bytes()).await.unwrap();
        stream
            .write_all(b"SSH-2.0-libssh2_1.11.0\r\n")
            .await
            .unwrap();
        stream.flush().await.unwrap();
        let mut answer = Vec::new();
        stream.read_to_end(&mut answer).await.unwrap();
        let answer = String::from_utf8(answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 403"), "{authority}: {answer}");
        assert!(
            answer.contains("x-puddle-blocked: ssh_unsupported"),
            "{authority}: {answer}"
        );
        assert!(
            answer.contains("SSH is not supported yet"),
            "{authority}: {answer}"
        );
    }
    let records = wait_for_records(&rig.store, attempts.len()).await;
    assert_eq!(records.len(), attempts.len(), "{records:?}");
    for record in &records {
        assert_eq!(record["decision"], "blocked", "{record}");
        assert_eq!(record["reason"], "ssh_unsupported", "{record}");
        assert!(
            record["pending_id"].is_null() && record["resolved_ip"].is_null(),
            "{record}"
        );
    }
    assert_eq!(
        rig.store.open_pending(None).unwrap().len(),
        0,
        "nothing in the inbox"
    );
    assert_eq!(rig.store.rules().len(), 1);
}

/// UDP the guest's capture rules refused is reported by the agent on its control stream and
/// audited as `blocked` with reason `udp_unsupported`: no inbox row, no rule, and nothing that is
/// not an IP address becomes a record. A line from the real agent's own message type, over the
/// real endpoint, into the real store.
#[tokio::test]
async fn refused_udp_is_audited_with_its_own_reason_and_never_pending() {
    use puddle_agent_proto::AgentMessage;
    use puddle_agent_proto::control::PREAMBLE;

    let rig = rig().await;
    let config = Config {
        target: Target::Unix(rig.route.endpoint().path().to_path_buf()),
        ..Config::default()
    };
    // The session lives as long as this value does.
    let upstream = puddle_agent::upstream::Upstream::new(&config);
    let mut control = upstream.open().await.unwrap();
    control.write_all(PREAMBLE).await.unwrap();
    for message in [
        AgentMessage::hello(),
        AgentMessage::udp_refused("192.0.2.9".to_owned(), 9999, 3),
        AgentMessage::udp_refused("2001:db8::9".to_owned(), 443, 1),
        // The guest's words are data: neither a name nor escape characters become a record.
        AgentMessage::udp_refused("evil.example".to_owned(), 53, 1),
        AgentMessage::udp_refused("\u{1b}[2J".to_owned(), 53, 1),
    ] {
        control
            .write_all(&message.to_line().unwrap())
            .await
            .unwrap();
        control.flush().await.unwrap();
    }
    let records = wait_for_records(&rig.store, 2).await;
    // Absence window: a late third connection record would still arrive after the two awaited.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let records = if connection_records(&rig.store).len() > records.len() {
        connection_records(&rig.store)
    } else {
        records
    };
    assert_eq!(records.len(), 2, "{records:?}");
    for (host, port) in [("192.0.2.9", 9999), ("2001:db8::9", 443)] {
        let record = records
            .iter()
            .find(|r| r["host"] == host && r["port"] == port)
            .unwrap_or_else(|| panic!("{host}:{port}: {records:?}"));
        assert_eq!(record["decision"], "blocked", "{record}");
        assert_eq!(record["reason"], "udp_unsupported", "{record}");
        assert!(
            record["pending_id"].is_null() && record["rule_id"].is_null(),
            "{record}"
        );
        assert!(record["resolved_ip"].is_null(), "never connected: {record}");
    }
    assert_eq!(rig.store.open_pending(None).unwrap().len(), 0);
    assert_eq!(rig.store.rules().len(), 0);
}

/// A guest that asks for 100 000 names nobody allowed is answered "waiting for approval" every
/// time, none of them is denied for the volume, the store lists no more than its cap, and the
/// request that was there first is still waiting and still answerable. Through the real agent,
/// proxy and rules engine.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_hundred_thousand_unknown_names_are_all_told_to_wait_and_the_store_stays_bounded() {
    const WORKERS: u32 = 16;
    const PER_WORKER: u32 = 6_250;
    let rig = Arc::new(rig().await);
    let agent = rig.agent.local_addr();
    let (code, _) = connect_via(agent, "first.test:443").await.unwrap();
    assert_eq!(code, 403);
    let first = rig.store.open_pending(Some(&workspace())).unwrap()[0].id;

    let mut workers = Vec::new();
    for worker in 0..WORKERS {
        workers.push(tokio::spawn(async move {
            let mut answers = std::collections::BTreeMap::<u16, u32>::new();
            for i in 0..PER_WORKER {
                let (code, mut conn) =
                    connect_via(agent, &format!("f{worker:x}-{i:x}.flood.test:443"))
                        .await
                        .unwrap();
                let mut body = String::new();
                conn.read_to_string(&mut body).await.unwrap();
                assert!(body.contains("for approval in Puddle"), "{body}");
                assert!(!body.contains("denied"), "{body}");
                *answers.entry(code).or_default() += 1;
            }
            answers
        }));
    }
    let mut refused = 0;
    for worker in workers {
        for (code, n) in worker.await.unwrap() {
            assert_eq!(code, 403, "only the waiting answer, never anything else");
            refused += n;
        }
    }
    assert_eq!(refused, WORKERS * PER_WORKER);

    let cap = usize::try_from(Limits::default().max_open_rows).unwrap();
    let (open, cpu) = cpu_time(|| rig.store.open_pending(Some(&workspace())).unwrap());
    eprintln!("open rows over the cap: {cpu:?} of CPU");
    // The bar is on this thread's CPU time, so the load of the flood tasks does not move it; it
    // catches a per-row query that scans (225 ms planted, 2 ms unplanted on Linux). This read is the
    // open rows alone, without the inbox grouping the sibling tests also time, so its bar is
    // tighter than theirs would need; the Windows clock ticks in 15.6 ms steps.
    assert!(
        cpu < Duration::from_millis(100),
        "the read used {cpu:?} of CPU"
    );
    assert_eq!(open.len(), cap);
    assert!(open.iter().any(|row| row.id == first));
    rig.store
        .resolve_pending(first, &Resolution::allow(), Actor::Cli)
        .unwrap();
}
