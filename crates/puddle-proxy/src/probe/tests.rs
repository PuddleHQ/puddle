// SPDX-License-Identifier: AGPL-3.0-or-later
use std::net::Ipv4Addr;

use super::*;

#[tokio::test]
async fn a_listening_port_is_reachable_and_a_closed_one_is_not() {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let open = listener.local_addr().unwrap().port();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let closed = socket.local_addr().unwrap().port();
    let probe = TcpPortProbe::new(Duration::from_secs(5), Duration::from_secs(60));
    assert!(probe.reachable("127.0.0.1", open).await);
    assert!(!probe.reachable("127.0.0.1", closed).await);
}

#[tokio::test(start_paused = true)]
async fn an_answer_is_remembered_until_exactly_its_time_is_up() {
    // Bound but not listening refuses connections; the same socket starts listening later,
    // so no port is ever given up and re-taken.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind((Ipv4Addr::LOCALHOST, 0).into()).unwrap();
    let port = socket.local_addr().unwrap().port();
    let probe = TcpPortProbe::new(Duration::from_secs(5), Duration::from_secs(60));
    assert!(!probe.reachable("127.0.0.1", port).await);
    let _listener = socket.listen(16).unwrap();
    // Remembered: the port now listens but the answer stands, up to the last instant before it expires.
    tokio::time::advance(Duration::from_secs(60) - Duration::from_millis(1)).await;
    assert!(!probe.reachable("127.0.0.1", port).await);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(probe.reachable("127.0.0.1", port).await);
}

#[test]
fn a_name_that_does_not_resolve_is_not_reachable() {
    // The probe's own time limit is 300 ms, so a resolver that is slow to say "no such name" (or
    // never does) costs the test that, not the resolver's retry time. The runtime is not dropped
    // by waiting for its blocking pool: the stuck system lookup runs there until it gives up.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let probe = TcpPortProbe::new(Duration::from_millis(300), Duration::from_secs(60));
    let reachable = runtime.block_on(probe.reachable("nothing.invalid", 22));
    runtime.shutdown_background();
    assert!(!reachable);
}
