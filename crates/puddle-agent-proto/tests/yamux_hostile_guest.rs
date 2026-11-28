// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a hostile guest can make the host's yamux session hold: streams that cost a task each, and
//! window grants that let the host queue bytes the guest never reads. Each test reports what it
//! measured in the failure message, so the cost per stream and per grant is on record.
#![expect(clippy::unwrap_used, reason = "a failed step fails the test")]

use std::time::Duration;

use futures_util::StreamExt;
use puddle_agent_proto::yamux::{MAX_STREAMS, STREAM_WINDOW, server_config};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio_yamux::{Session, config::INITIAL_STREAM_WINDOW};

const WINDOW_UPDATE: u8 = 1;
const SYN: u16 = 1;
const ACK: u16 = 2;
const RST: u16 = 8;

fn header(kind: u8, flags: u16, stream: u32, length: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[1] = kind;
    h[2..4].copy_from_slice(&flags.to_be_bytes());
    h[4..8].copy_from_slice(&stream.to_be_bytes());
    h[8..12].copy_from_slice(&length.to_be_bytes());
    h
}

/// Resident set size of this process in KiB; 0 where there is no `/proc` (Windows), so the
/// message reports no figure there and the assertions are the same.
fn rss_kib() -> u64 {
    let Ok(statm) = std::fs::read_to_string("/proc/self/statm") else {
        return 0;
    };
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4
}

/// A guest opens `opened` streams with a SYN each and says nothing else. Returns how many the
/// host's session accepted and how many it reset, as the guest sees them: every SYN is answered
/// with an ack or a reset.
async fn flood(opened: u32) -> (usize, usize, u64) {
    let (peer, ours) = duplex(1 << 20);
    let mut session = Session::new_server(ours, server_config());
    // The accepted streams are held, as the host's stream tasks hold them.
    let driver = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(Ok(stream)) = session.next().await {
            held.push(stream);
        }
    });
    let (mut rx, mut tx) = tokio::io::split(peer);
    let reader = tokio::spawn(async move {
        let (mut acked, mut reset) = (0, 0);
        let mut h = [0u8; 12];
        while acked + reset < opened as usize {
            rx.read_exact(&mut h).await.unwrap();
            let flags = u16::from_be_bytes([h[2], h[3]]);
            if flags & RST != 0 {
                reset += 1;
            } else if flags & ACK != 0 {
                acked += 1;
            }
        }
        (acked, reset)
    });
    let before = rss_kib();
    for n in 0..opened {
        tx.write_all(&header(WINDOW_UPDATE, SYN, 2 * n + 1, 0))
            .await
            .unwrap();
    }
    let (accepted, resets) = tokio::time::timeout(Duration::from_secs(20), reader)
        .await
        .unwrap()
        .unwrap();
    let held_kib = rss_kib().saturating_sub(before);
    driver.abort();
    (accepted, resets, held_kib)
}

#[tokio::test]
async fn a_guest_opening_streams_without_end_is_held_to_the_stream_cap() {
    let extra = 1000;
    let (accepted, resets, held_kib) = flood(u32::try_from(MAX_STREAMS).unwrap() + extra).await;
    assert_eq!(
        accepted, MAX_STREAMS,
        "{held_kib} KiB resident for the held streams"
    );
    assert_eq!(
        resets,
        usize::try_from(extra).unwrap(),
        "each stream past the cap is reset"
    );
}

#[tokio::test]
async fn a_guest_granting_more_window_than_it_reads_cannot_make_the_host_queue_it() {
    let (peer, ours) = duplex(1 << 16);
    let mut session = Session::new_server(ours, server_config());
    let (stream_tx, stream_rx) = tokio::sync::oneshot::channel();
    let driver = tokio::spawn(async move {
        let mut stream_tx = Some(stream_tx);
        let mut held = Vec::new();
        while let Some(Ok(stream)) = session.next().await {
            match stream_tx.take() {
                Some(tx) => drop(tx.send(stream)),
                None => held.push(stream),
            }
        }
    });
    let (_rx, mut tx) = tokio::io::split(peer);
    // The guest opens a stream, grants nearly 4 GiB and never reads a byte.
    tx.write_all(&header(WINDOW_UPDATE, SYN, 1, 0))
        .await
        .unwrap();
    tx.write_all(&header(
        WINDOW_UPDATE,
        0,
        1,
        u32::MAX - INITIAL_STREAM_WINDOW,
    ))
    .await
    .unwrap();
    let mut stream = stream_rx.await.unwrap();

    // The host's upstream sends 1 MiB at a time until a write waits or 512 MiB are queued.
    let before = rss_kib();
    let chunk = vec![0u8; 1 << 20];
    let mut queued = 0usize;
    while queued < 512 << 20 {
        let step = tokio::time::timeout(Duration::from_millis(300), stream.write(&chunk)).await;
        match step {
            Ok(Ok(n)) => queued += n,
            _ => break,
        }
    }
    let held_kib = rss_kib().saturating_sub(before);
    assert!(
        queued >= STREAM_WINDOW as usize,
        "the host queued only {queued} bytes: the window is cut too far"
    );
    assert!(
        queued <= (STREAM_WINDOW + INITIAL_STREAM_WINDOW) as usize,
        "the host queued {queued} bytes ({held_kib} KiB resident) for a guest that reads none"
    );
    driver.abort();
}
