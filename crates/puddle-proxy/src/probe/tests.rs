// SPDX-License-Identifier: GPL-3.0-or-later
use std::net::Ipv4Addr;

use super::*;

#[tokio::test]
async fn a_listening_port_is_reachable_and_a_closed_one_is_not() {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let open = listener.local_addr().unwrap().port();
    let closed = {
        let l = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        l.local_addr().unwrap().port()
    };
    let probe = TcpPortProbe::new(Duration::from_secs(5), Duration::from_secs(60));
    assert!(probe.reachable("127.0.0.1", open).await);
    assert!(!probe.reachable("127.0.0.1", closed).await);
}

#[tokio::test(start_paused = true)]
async fn an_answer_is_remembered_until_exactly_its_time_is_up() {
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let probe = TcpPortProbe::new(Duration::from_secs(5), Duration::from_secs(60));
    assert!(probe.reachable("127.0.0.1", port).await);
    drop(listener);
    // Remembered: the port closed but the answer stands, up to the last instant before it expires.
    tokio::time::advance(Duration::from_secs(60) - Duration::from_millis(1)).await;
    assert!(probe.reachable("127.0.0.1", port).await);
    tokio::time::advance(Duration::from_millis(1)).await;
    assert!(!probe.reachable("127.0.0.1", port).await);
}

#[tokio::test]
async fn a_name_that_does_not_resolve_is_not_reachable() {
    let probe = TcpPortProbe::default();
    assert!(!probe.reachable("nothing.invalid", 22).await);
}
