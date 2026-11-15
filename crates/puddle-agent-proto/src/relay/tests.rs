// SPDX-License-Identifier: AGPL-3.0-or-later
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio::net::TcpListener;

use super::*;

/// A stream that takes every byte, then fails reads like a yamux stream that got RST.
struct ResetStream;

impl AsyncRead for ResetStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
    }
}

impl AsyncWrite for ResetStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(buf.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

async fn pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (ours, _) = listener.accept().await.unwrap();
    (peer, ours)
}

#[tokio::test]
async fn a_reset_on_the_stream_reaches_the_tcp_peer_as_a_reset_not_a_clean_eof() {
    let (mut peer, mut ours) = pair().await;
    assert!(splice(&mut ours, &mut ResetStream).await.is_err());
    drop(ours);
    let mut got = Vec::new();
    let read = peer.read_to_end(&mut got).await;
    assert_eq!(
        read.map_err(|e| e.kind()).err(),
        Some(io::ErrorKind::ConnectionReset)
    );
}

#[tokio::test]
async fn a_clean_close_on_both_sides_stays_clean_and_counts_bytes() {
    let (mut peer, mut ours) = pair().await;
    let (mut near, mut far) = tokio::io::duplex(1024);
    let echo = tokio::spawn(async move {
        let mut buf = Vec::new();
        far.read_to_end(&mut buf).await.unwrap();
        far.write_all(&buf).await.unwrap();
        far.shutdown().await.unwrap();
    });
    let relay = tokio::spawn(async move { splice(&mut ours, &mut near).await });
    peer.write_all(b"hello").await.unwrap();
    peer.shutdown().await.unwrap();
    let mut back = Vec::new();
    peer.read_to_end(&mut back).await.unwrap();
    assert_eq!(back, b"hello");
    assert_eq!(relay.await.unwrap().unwrap(), (5, 5));
    echo.await.unwrap();
}
