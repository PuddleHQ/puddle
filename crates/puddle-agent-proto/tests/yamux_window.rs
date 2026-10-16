// SPDX-License-Identifier: GPL-3.0-or-later
//! What a session does while a stream is not read: a peer that sends more than the stream's
//! receive window gets that stream reset and the session goes on serving the others (a session
//! that stopped reading its socket instead would stall every stream on it); a peer that stays
//! within the window loses nothing, its close included.
#![expect(clippy::unwrap_used, reason = "a failed step fails the test")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use puddle_agent_proto::yamux::{STREAM_WINDOW, server_config};
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf, duplex};
use tokio_yamux::Session;

const DATA: u8 = 0;
const WINDOW_UPDATE: u8 = 1;
const SYN: u16 = 1;
const ACK: u16 = 2;
const FIN: u16 = 4;
const RST: u16 = 8;

fn header(kind: u8, flags: u16, stream: u32, length: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[1] = kind;
    h[2..4].copy_from_slice(&flags.to_be_bytes());
    h[4..8].copy_from_slice(&stream.to_be_bytes());
    h[8..12].copy_from_slice(&length.to_be_bytes());
    h
}

/// Reads frames until one for `stream` carries `flag`. Data frames are never sent to this peer,
/// so only headers come back.
async fn wait_for(rx: &mut ReadHalf<tokio::io::DuplexStream>, stream: u32, flag: u16) {
    let found = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let mut h = [0u8; 12];
            rx.read_exact(&mut h).await.unwrap();
            let flags = u16::from_be_bytes([h[2], h[3]]);
            if u32::from_be_bytes([h[4], h[5], h[6], h[7]]) == stream && flags & flag != 0 {
                return;
            }
        }
    })
    .await;
    assert!(
        found.is_ok(),
        "no frame for stream {stream} with flag {flag} in 20 s"
    );
}

async fn open(
    tx: &mut WriteHalf<tokio::io::DuplexStream>,
    rx: &mut ReadHalf<tokio::io::DuplexStream>,
    stream: u32,
) {
    tx.write_all(&header(WINDOW_UPDATE, SYN, stream, 0))
        .await
        .unwrap();
    wait_for(rx, stream, ACK).await;
}

#[tokio::test]
async fn a_stream_sent_past_its_window_while_unread_is_reset_and_the_session_goes_on() {
    let (peer, ours) = duplex(1 << 20);
    let mut session = Session::new_server(ours, server_config());
    // The accepted streams are kept and never read: the application is slow.
    let held = tokio::spawn(async move {
        let mut streams = Vec::new();
        while let Some(Ok(stream)) = session.next().await {
            streams.push(stream);
        }
    });
    let (mut rx, mut tx) = {
        let (r, w) = tokio::io::split(peer);
        (r, w)
    };
    open(&mut tx, &mut rx, 1).await;

    // Twice the window in 1 MiB frames, from a peer that ignores the window it was given.
    let body = vec![0u8; 1 << 20];
    let frames = 2 * (STREAM_WINDOW as usize >> 20);
    let sender = tokio::spawn(async move {
        for _ in 0..frames {
            tx.write_all(&header(DATA, 0, 1, 1 << 20)).await.unwrap();
            tx.write_all(&body).await.unwrap();
        }
        tx
    });
    // Frames come back as window updates for stream 1 (acknowledgements of nothing) and then
    // the reset; reading them is what lets the peer's writes through the pipe.
    wait_for(&mut rx, 1, RST).await;
    let mut tx = tokio::time::timeout(Duration::from_secs(20), sender)
        .await
        .unwrap()
        .unwrap();

    // The session still opens another stream.
    open(&mut tx, &mut rx, 3).await;
    held.abort();
}

#[tokio::test]
async fn a_stream_holding_a_full_window_and_a_close_while_unread_still_reads_to_the_end() {
    let (peer, ours) = duplex(1 << 20);
    let mut session = Session::new_server(ours, server_config());
    let (stream_tx, stream_rx) = tokio::sync::oneshot::channel();
    let driver = tokio::spawn(async move {
        let mut stream_tx = Some(stream_tx);
        while let Some(Ok(stream)) = session.next().await {
            if let Some(tx) = stream_tx.take() {
                tx.send(stream).ok();
            }
        }
    });
    let (mut rx, mut tx) = tokio::io::split(peer);
    open(&mut tx, &mut rx, 1).await;
    let stream = stream_rx.await.unwrap();

    // Frames without data first, so the stream's frame channel is full of them and the window's
    // worth of data that follows waits in the session, then the peer closes its side. All of it is
    // within the window the stream granted.
    let reset = Arc::new(AtomicBool::new(false));
    let seen = reset.clone();
    let drain = tokio::spawn(async move {
        let mut h = [0u8; 12];
        while rx.read_exact(&mut h).await.is_ok() {
            if u16::from_be_bytes([h[2], h[3]]) & RST != 0 {
                seen.store(true, Ordering::SeqCst);
            }
        }
    });
    let window = STREAM_WINDOW as usize;
    let body = vec![7u8; 1 << 20];
    let sender = tokio::spawn(async move {
        for _ in 0..16 {
            tx.write_all(&header(WINDOW_UPDATE, 0, 1, 0)).await.unwrap();
        }
        for _ in 0..window >> 20 {
            tx.write_all(&header(DATA, 0, 1, 1 << 20)).await.unwrap();
            tx.write_all(&body).await.unwrap();
        }
        tx.write_all(&header(DATA, FIN, 1, 0)).await.unwrap();
        tx
    });
    let _tx = tokio::time::timeout(Duration::from_secs(20), sender)
        .await
        .unwrap()
        .unwrap();

    let mut stream = stream;
    let mut got = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut got))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        got.len(),
        window,
        "the reader got the whole window, then the close"
    );
    assert!(got.iter().all(|b| *b == 7));
    assert!(
        !reset.load(Ordering::SeqCst),
        "the session reset a stream whose peer stayed within its window"
    );
    driver.abort();
    drain.abort();
}
