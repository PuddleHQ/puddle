// SPDX-License-Identifier: GPL-3.0-or-later
//! A client that sends its request, shuts down its write side and then reads a response bigger
//! than one yamux stream window, through the proxy's spliced tunnel (a name no termination binds).
#![expect(clippy::unwrap_used, reason = "tests fail by panicking")]
use crate::terminate_support;

use std::time::Duration;

use terminate_support::{Pki, RigBuilder};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const MIB: usize = 1024 * 1024;

/// Reads the whole request up to the client's end of stream, then answers with `mib` MiB.
async fn answer_after_eof(mib: usize) -> std::net::SocketAddr {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut tcp, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut request = Vec::new();
                tcp.read_to_end(&mut request).await.unwrap();
                let chunk = vec![b'x'; 256 * 1024];
                for _ in 0..mib * 4 {
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
async fn a_half_closed_tunnel_still_delivers_a_download_larger_than_the_stream_window() {
    const DOWNLOAD_MIB: usize = 40;
    let server = answer_after_eof(DOWNLOAD_MIB).await;
    let rig = RigBuilder::new(&Pki::new())
        .name("plain.test", server)
        .allow(vec!["plain.test"])
        .build();
    let mut guest = rig.guest().await;
    let (code, mut tunnel) = guest.connect_to("plain.test:443").await;
    assert_eq!(code, 200);
    tunnel.write_all(b"GET /big\r\n\r\n").await.unwrap();
    tunnel.get_mut().shutdown().await.unwrap();
    let mut total = 0;
    let mut buf = vec![0_u8; 256 * 1024];
    let read = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let n = tunnel.read(&mut buf).await.unwrap();
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
