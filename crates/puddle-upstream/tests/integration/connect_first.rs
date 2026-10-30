// SPDX-License-Identifier: GPL-3.0-or-later
//! Connecting to a destination with several addresses, as a client that races them does: one
//! address whose connection attempts are lost (a route that drops packets, a server whose backlog
//! is full) must not hold the connection up for the whole connect timeout.
#![expect(clippy::unwrap_used, reason = "tests fail by panicking")]

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use puddle_upstream::connect_first;
use tokio::net::{TcpListener, TcpSocket, TcpStream};

/// A listener that takes no more connections: its backlog of one is full and nothing accepts, so
/// on Linux and macOS the kernel drops further connection attempts, as a lossy route would (on
/// Windows they are refused, which the test also passes). The returned streams hold the backlog.
async fn full_listener() -> (SocketAddr, TcpListener, Vec<TcpStream>) {
    let socket = TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = socket.local_addr().unwrap();
    let listener = socket.listen(1).unwrap();
    let mut held = Vec::new();
    for _ in 0..4 {
        match tokio::time::timeout(Duration::from_millis(200), TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => held.push(stream),
            _ => break,
        }
    }
    (addr, listener, held)
}

#[tokio::test]
async fn an_address_that_drops_connection_attempts_does_not_hold_up_the_next_one() {
    let (lost, _listener, _held) = full_listener().await;
    let good = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let good_addr = good.local_addr().unwrap();
    let per_address = Duration::from_secs(10);
    let started = Instant::now();
    let (_, addr) = connect_first(&[lost, good_addr], per_address)
        .await
        .unwrap();
    let took = started.elapsed();
    assert_eq!(addr, good_addr);
    assert!(
        took < per_address / 2,
        "the second address was tried only after {took:?}"
    );
}

#[tokio::test]
async fn an_address_that_never_answers_fails_after_its_own_timeout() {
    let (lost, _listener, _held) = full_listener().await;
    let started = Instant::now();
    let err = connect_first(&[lost], Duration::from_millis(300))
        .await
        .unwrap_err();
    // Dropped attempts time out (Linux, macOS); Windows refuses them instead.
    assert!(
        matches!(
            err.kind(),
            std::io::ErrorKind::TimedOut | std::io::ErrorKind::ConnectionRefused
        ),
        "{err:?}"
    );
    // flake-ok: a connect nobody answers waits for the kernel's own timeout (75 s on macOS, 127 s on
    // Linux), so 5 s tells that from the 300 ms limit; 0.31 s measured under load, 16x under the bar.
    assert!(started.elapsed() < Duration::from_secs(5));
}
