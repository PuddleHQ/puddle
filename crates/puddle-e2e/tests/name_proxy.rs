// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake internet's company proxy: a tunnel to a name it holds goes to that name's server, and
//! any other name is connected to as it stands.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use puddle_e2e::lab::{GitHost, NameProxy, Versions, git_available};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Sends `CONNECT authority` to the proxy and returns the stream and the status line.
async fn connect(proxy: &NameProxy, authority: &str) -> (TcpStream, String) {
    let mut conn = TcpStream::connect(("127.0.0.1", proxy.port()))
        .await
        .unwrap();
    conn.write_all(format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        conn.read_exact(&mut byte).await.unwrap();
        head.push(byte[0]);
    }
    let line = String::from_utf8_lossy(&head)
        .lines()
        .next()
        .unwrap()
        .to_owned();
    (conn, line)
}

#[tokio::test]
async fn a_held_name_goes_to_its_server_and_any_other_name_is_connected_to_as_it_stands() {
    if !git_available() {
        return;
    }
    let host = GitHost::start("held.test", Versions::Http1).await;
    let elsewhere = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let elsewhere_port = elsewhere.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut conn, _) = elsewhere.accept().await.unwrap();
        conn.write_all(b"hello from elsewhere").await.unwrap();
    });
    let proxy = NameProxy::start(&[("held.test", host.addr())]).await;

    // A held name: the TLS server of the fake host answers (a ClientHello it cannot read ends in
    // an alert or a close, which is an answer from it and not from `elsewhere`).
    let (mut conn, line) = connect(&proxy, "held.test:443").await;
    assert!(line.starts_with("HTTP/1.1 200"), "{line}");
    conn.write_all(b"not TLS\r\n\r\n").await.unwrap();
    let mut got = Vec::new();
    let _ = conn.read_to_end(&mut got).await;
    assert!(!String::from_utf8_lossy(&got).contains("hello from elsewhere"));

    // A client that goes away before it finishes its request is forgotten: the proxy serves the
    // next connection and the `connects` list below holds no entry for it, whenever it notices.
    drop(
        TcpStream::connect(("127.0.0.1", proxy.port()))
            .await
            .unwrap(),
    );

    // Another name: connected to as it stands.
    let (mut conn, line) = connect(&proxy, &format!("127.0.0.1:{elsewhere_port}")).await;
    assert!(line.starts_with("HTTP/1.1 200"), "{line}");
    let mut got = String::new();
    conn.read_to_string(&mut got).await.unwrap();
    assert_eq!(got, "hello from elsewhere");

    // A name nothing answers on: 502.
    let dead = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let dead_port = dead.local_addr().unwrap().port();
    drop(dead);
    let (_, line) = connect(&proxy, &format!("127.0.0.1:{dead_port}")).await;
    assert!(line.starts_with("HTTP/1.1 502"), "{line}");
    assert_eq!(
        proxy.connects(),
        [
            "held.test:443".to_owned(),
            format!("127.0.0.1:{elsewhere_port}"),
            format!("127.0.0.1:{dead_port}")
        ]
    );
}
