// SPDX-License-Identifier: GPL-3.0-or-later
//! A plain-HTTP response bigger than one yamux stream window, forwarded by the proxy (which reads
//! nothing more from the guest once the request is in), arrives whole. The client keeps its
//! sending side open, as curl and apt do.
#![expect(clippy::unwrap_used, reason = "tests fail by panicking")]
mod terminate_support;

use std::time::Duration;

use terminate_support::{Pki, RigBuilder};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const MIB: usize = 1024 * 1024;
const DOWNLOAD_MIB: usize = 40;

/// Answers one request head with `DOWNLOAD_MIB` MiB and a `Content-Length`, then closes.
async fn sized_server() -> std::net::SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut tcp = BufReader::new(tcp);
                let mut line = String::new();
                while tcp.read_line(&mut line).await.unwrap() > 2 {
                    line.clear();
                }
                let tcp = tcp.get_mut();
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    DOWNLOAD_MIB * MIB
                );
                tcp.write_all(head.as_bytes()).await.unwrap();
                let chunk = vec![b'x'; MIB];
                for _ in 0..DOWNLOAD_MIB {
                    if tcp.write_all(&chunk).await.is_err() {
                        return;
                    }
                }
                let _ = tcp.shutdown().await;
            });
        }
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_plain_http_download_larger_than_the_stream_window_arrives_whole() {
    let server = sized_server().await;
    let rig = RigBuilder::new(&Pki::new())
        .name("plain.test", server)
        .allow(vec!["plain.test"])
        .build();
    let guest = rig.guest().await;
    let mut stream = guest.control.clone().open_stream().await.unwrap();
    stream
        .write_all(b"GET http://plain.test/big HTTP/1.1\r\nHost: plain.test\r\n\r\n")
        .await
        .unwrap();
    let mut response = BufReader::new(stream);
    let mut head = Vec::new();
    loop {
        let mut line = String::new();
        response.read_line(&mut line).await.unwrap();
        if line == "\r\n" {
            break;
        }
        head.push(line);
    }
    assert_eq!(
        head,
        [
            "HTTP/1.1 200 OK\r\n".to_owned(),
            format!("Content-Length: {}\r\n", DOWNLOAD_MIB * MIB)
        ]
    );
    let mut total = 0;
    let mut buf = vec![0_u8; 256 * 1024];
    let read = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let n = response.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            total += n;
        }
    })
    .await;
    assert!(
        read.is_ok(),
        "the download stalled after {} MiB of {DOWNLOAD_MIB}",
        total / MIB
    );
    assert_eq!(total, DOWNLOAD_MIB * MIB);
}
