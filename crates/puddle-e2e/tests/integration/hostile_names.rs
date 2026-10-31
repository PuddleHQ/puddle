// SPDX-License-Identifier: GPL-3.0-or-later
//! End to end, no VM: a guest names destinations with terminal escapes, homographs, bidi
//! controls, full-width forms and over-long names, through the real guest agent, the real proxy and
//! the real SQLite rules and audit. A name is either refused before any rule with the reason in the
//! answer, or it waits as a pending row under its ASCII form (punycode for a non-ASCII name). Every
//! audit line parses as JSON and holds no raw control character.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use puddle_agent::config::Target;
use puddle_agent::{Agent, Config};
use puddle_ipc::IpcRoot;
use puddle_proxy::testing::{AnyAddress, StaticResolver};
use puddle_proxy::{Proxy, Route};
use puddle_store::{Limits, ManualClock, Store};
use puddle_types::{NullSink, WorkspaceName};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const NOW: u64 = 1_800_000_000_000;

struct Rig {
    store: Arc<Store>,
    agent: Agent,
    _route: Route,
    _root: IpcRoot,
}

async fn rig() -> Rig {
    let store = Arc::new(
        Store::open_in_memory(Arc::new(ManualClock::new(NOW)), Limits::default()).unwrap(),
    );
    let proxy = Arc::new(
        Proxy::new(store.clone(), Arc::new(NullSink))
            .with_connection_log(store.clone())
            .with_resolver(Arc::new(StaticResolver::new()))
            .with_address_check(Arc::new(AnyAddress)),
    );
    let root = IpcRoot::new().unwrap();
    let route = proxy.serve_route(
        root.listen().unwrap(),
        WorkspaceName::new("e2e-names").unwrap(),
    );
    let agent = Agent::start(Config {
        listen: SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), 0),
        target: Target::Unix(route.endpoint().path().to_path_buf()),
        oom: None,
        ..Config::default()
    })
    .await
    .unwrap();
    Rig {
        store,
        agent,
        _route: route,
        _root: root,
    }
}

/// What `CONNECT <authority>` through the agent got: the status and the answer's text.
async fn connect_via(agent: SocketAddr, authority: &[u8]) -> io::Result<(u16, String)> {
    let mut conn = TcpStream::connect(agent).await?;
    let mut request = b"CONNECT ".to_vec();
    request.extend_from_slice(authority);
    request.extend_from_slice(b" HTTP/1.1\r\n\r\n");
    conn.write_all(&request).await?;
    let mut raw = Vec::new();
    // The proxy ends the stream after a refusal or a pending answer.
    tokio::time::timeout(Duration::from_secs(5), conn.read_to_end(&mut raw))
        .await
        .map_err(|_| io::Error::other("no answer"))??;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let code = text
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| io::Error::other(format!("bad status line {text:?}")))?;
    Ok((code, text))
}

/// Every audit line, parsed, waiting up to 5 s until `connections` of them are connection records.
fn audit(store: &Store, connections: usize) -> Vec<(String, Value)> {
    puddle_test_kit::eventually_blocking(
        &format!("{connections} connection audit records"),
        Duration::from_secs(5),
        || {
            let lines = store.audit_lines(0, 100_000).unwrap();
            let parsed: Vec<(String, Value)> = lines
                .into_iter()
                .map(|(_, line)| {
                    let value = serde_json::from_str::<Value>(&line)
                        .map_err(|e| format!("an audit line that is not JSON ({e}): {line:?}"))
                        .unwrap();
                    (line, value)
                })
                .collect();
            let seen = parsed
                .iter()
                .filter(|(_, v)| v["type"] == "connection")
                .count();
            (seen >= connections).then_some(parsed)
        },
    )
}

/// (what the guest sent, the answer's status, text the answer says, the row's host if any)
fn hostile_names() -> Vec<(String, u16, &'static str, Option<String>)> {
    let long_ok = format!("{0}.{0}.{0}.{1}", "a".repeat(63), "b".repeat(61));
    assert_eq!(long_ok.len(), 253);
    let too_long = format!("{0}.{0}.{0}.{1}", "a".repeat(63), "b".repeat(62));
    assert_eq!(too_long.len(), 254);
    vec![
        (
            "\u{1b}[2Kevil.test:443".into(),
            400,
            "control character in head",
            None,
        ),
        (
            "evil\r.test:443".into(),
            400,
            "control character in head",
            None,
        ),
        (
            "a\0b.test:443".into(),
            400,
            "control character in head",
            None,
        ),
        ("a%00b.test:443".into(), 400, "not a valid host name", None),
        ("a b.test:443".into(), 400, "malformed request line", None),
        (
            "а\u{202e}pple.com:443".into(),
            400,
            "not a valid host name",
            None,
        ),
        (
            too_long.clone() + ":443",
            400,
            "is longer than 253 characters",
            None,
        ),
        (
            "аррӏе.com:443".into(),
            403,
            "pending",
            Some("xn--80ak6aa92e.com".into()),
        ),
        (
            "bücher.example:443".into(),
            403,
            "pending",
            Some("xn--bcher-kva.example".into()),
        ),
        (
            "ＥＶＩＬ.example:443".into(),
            403,
            "pending",
            Some("evil.example".into()),
        ),
        (
            long_ok.clone() + ":443",
            403,
            "pending",
            Some(long_ok.clone()),
        ),
    ]
}

#[tokio::test]
async fn hostile_hg13_hostile_names_are_refused_or_waiting_under_their_ascii_form() {
    let rig = rig().await;
    let cases = hostile_names();
    let mut expected_rows = Vec::new();
    for (sent, code, says, row) in &cases {
        let (got, text) = connect_via(rig.agent.local_addr(), sent.as_bytes())
            .await
            .unwrap();
        assert_eq!(got, *code, "{sent:?}: {text}");
        assert!(
            text.contains(says),
            "{sent:?}: the answer must say {says:?}: {text:?}"
        );
        if let Some(row) = row {
            expected_rows.push(row.clone());
        }
    }
    let rows = rig.store.open_pending(None).unwrap();
    let mut hosts: Vec<String> = rows.iter().map(|r| r.host.to_string()).collect();
    hosts.sort();
    expected_rows.sort();
    assert_eq!(
        hosts, expected_rows,
        "a refused name leaves no row, the others wait under ASCII"
    );
    assert!(hosts.iter().all(|h| {
        h.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    }));
    // The audit: one connection record per waiting row; every line is JSON (the helper checked) and
    // none carries a raw control character or a character outside printable ASCII in a host.
    let lines = audit(&rig.store, expected_rows.len());
    for (line, value) in &lines {
        assert!(
            !line.bytes().any(|b| b < 0x20 || b == 0x7f),
            "a raw control character in {line:?}"
        );
        if let Some(host) = value["host"].as_str() {
            assert!(
                host.is_ascii(),
                "{host:?} in the audit is not ASCII: {line}"
            );
        }
    }
    let mut audited: Vec<String> = lines
        .iter()
        .filter(|(_, v)| v["type"] == "connection")
        .filter_map(|(_, v)| v["host"].as_str().map(str::to_owned))
        .collect();
    audited.sort();
    audited.dedup();
    assert_eq!(audited, expected_rows);
}
