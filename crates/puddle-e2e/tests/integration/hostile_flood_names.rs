// SPDX-License-Identifier: GPL-3.0-or-later
//! HG-12 through the proxy, no VM: a guest asks to CONNECT to 100 000 names nothing allows. None is
//! refused for volume (each is answered as waiting, never as denied), the store keeps its cap of
//! open rows, the inbox still answers in under 200 ms, a request that was waiting first is still
//! there and still answerable, and every audit line parses. The real proxy, SQLite rules and audit
//! are behind a route; the guest is the agent's own yamux client.

use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::stream;
use puddle_agent_proto::tokio_yamux::{Control, Session};
use puddle_agent_proto::yamux::client_config;
use puddle_ipc::IpcRoot;
use puddle_proxy::Proxy;
use puddle_proxy::testing::{AnyAddress, StaticResolver};
use puddle_store::{Actor, Limits, ManualClock, PendingState, Resolution, Store};
use puddle_test_kit::cpu_time;
use puddle_types::{NullSink, WorkspaceName};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const NOW: u64 = 1_800_000_000_000;
const FLOOD: usize = 100_000;
const AT_ONCE: usize = 500;

/// What `CONNECT name:443` was answered: the status and the `x-puddle-decision` value.
async fn ask(mut control: Control, name: String) -> Option<(u16, String)> {
    let mut stream = control.open_stream().await.ok()?;
    stream
        .write_all(format!("CONNECT {name}:443 HTTP/1.1\r\n\r\n").as_bytes())
        .await
        .ok()?;
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut raw))
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&raw).into_owned();
    let code = text.split(' ').nth(1)?.parse().ok()?;
    let decision = text
        .lines()
        .find_map(|l| l.strip_prefix("x-puddle-decision: "))?
        .to_owned();
    Some((code, decision))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_hg12_a_hundred_thousand_unmatched_names_wait_and_are_never_denied_for_volume() {
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
        WorkspaceName::new("e2e-flood").unwrap(),
    );
    let conn = puddle_ipc::connect(route.endpoint().path()).await.unwrap();
    let mut session = Session::new_client(conn, client_config());
    let control = session.control();
    let driver = tokio::spawn(async move { while let Some(Ok(_)) = session.next().await {} });

    // One real request first, then the flood.
    let (code, decision) = ask(control.clone(), "registry.example".into())
        .await
        .unwrap();
    assert_eq!((code, decision.as_str()), (403, "pending"));
    let real = store.open_pending(None).unwrap();
    assert_eq!(real.len(), 1);
    let real = real[0].id;

    let mut waiting = 0_usize;
    let mut other = Vec::new();
    let mut results = stream::iter(0..FLOOD)
        .map(|n| ask(control.clone(), format!("n{n}.flood.example")))
        .buffer_unordered(AT_ONCE);
    while let Some(answer) = results.next().await {
        match answer {
            Some((403, d)) if d == "pending" => waiting += 1,
            answer => {
                if other.len() < 3 {
                    other.push(answer);
                }
            }
        }
    }
    drop(results);
    assert_eq!((waiting, other.len()), (FLOOD, 0), "{other:?}");

    let cap = usize::try_from(Limits::default().max_open_rows).unwrap();
    let ((rows, groups), cpu) =
        cpu_time(|| (store.open_pending(None).unwrap(), store.inbox().unwrap()));
    eprintln!("inbox over the capped open rows: {cpu:?} of CPU");
    // The bar is on this thread's CPU time, so the load of the flood tasks does not move it; it
    // catches a per-row rescan of the open rows, which costs seconds (2.2 s planted). The same
    // read took 5 ms on Linux; the Windows clock ticks in 15.6 ms steps, so 100 ms is far above it.
    assert!(
        cpu < Duration::from_millis(200),
        "the inbox used {cpu:?} of CPU"
    );
    assert_eq!(rows.len(), cap, "the open rows stay at the cap");
    assert_eq!(groups.iter().map(|g| g.rows.len()).sum::<usize>(), cap);
    // The request that waited first is still there and still answerable.
    assert_eq!(store.pending(real).unwrap().state, PendingState::Requested);
    store
        .resolve_pending(real, &Resolution::allow(), Actor::Ui)
        .unwrap();
    let audit: Vec<String> = store
        .audit_lines(0, 1_000_000)
        .unwrap()
        .into_iter()
        .map(|(_, line)| line)
        .collect();
    for line in &audit {
        serde_json::from_str::<serde_json::Value>(line).unwrap();
        assert!(!line.contains("suppressed"), "{line}");
    }
    driver.abort();
}
