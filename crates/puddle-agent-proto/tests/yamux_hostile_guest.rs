// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a hostile guest can make the host's yamux session hold: streams that cost a task each, and
//! window grants that let the host queue bytes the guest never reads. Each test prints what it
//! measured (`--nocapture`) so the cost per stream and per grant is on record.
#![expect(clippy::unwrap_used, reason = "a failed step fails the test")]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use puddle_agent_proto::yamux::{MAX_STREAMS, STREAM_WINDOW, server_config};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio_yamux::{Session, config::INITIAL_STREAM_WINDOW};

const WINDOW_UPDATE: u8 = 1;
const SYN: u16 = 1;
const RST: u16 = 8;

fn header(kind: u8, flags: u16, stream: u32, length: u32) -> [u8; 12] {
    let mut h = [0u8; 12];
    h[1] = kind;
    h[2..4].copy_from_slice(&flags.to_be_bytes());
    h[4..8].copy_from_slice(&stream.to_be_bytes());
    h[8..12].copy_from_slice(&length.to_be_bytes());
    h
}

/// Resident set size of this process in KiB.
fn rss_kib() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").unwrap();
    let pages: u64 = statm.split_whitespace().nth(1).unwrap().parse().unwrap();
    pages * 4
}

/// A guest opens `opened` streams with a SYN each and says nothing else. Returns how many the
/// host's session accepted and how many it reset.
async fn flood(opened: u32) -> (usize, usize) {
    let (peer, ours) = duplex(1 << 20);
    let mut session = Session::new_server(ours, server_config());
    let accepted = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&accepted);
    let driver = tokio::spawn(async move {
        let mut held = Vec::new();
        while let Some(Ok(stream)) = session.next().await {
            held.push(stream);
            counted.fetch_add(1, Ordering::SeqCst);
        }
    });
    let (mut rx, mut tx) = tokio::io::split(peer);
    let resets = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&resets);
    let reader = tokio::spawn(async move {
        let mut h = [0u8; 12];
        while rx.read_exact(&mut h).await.is_ok() {
            if u16::from_be_bytes([h[2], h[3]]) & RST != 0 {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    let before = rss_kib();
    for n in 0..opened {
        tx.write_all(&header(WINDOW_UPDATE, SYN, 2 * n + 1, 0))
            .await
            .unwrap();
    }
    // Every SYN is answered with an ack or a reset, so the two counts add up to what was sent.
    let all_answered = tokio::time::timeout(Duration::from_secs(20), async {
        while accepted.load(Ordering::SeqCst) + resets.load(Ordering::SeqCst) < opened as usize {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let held_kib = rss_kib().saturating_sub(before);
    let (accepted, resets) = (
        accepted.load(Ordering::SeqCst),
        resets.load(Ordering::SeqCst),
    );
    eprintln!(
        "flood of {opened} SYN-only streams: {accepted} accepted, {resets} reset, \
         about {held_kib} KiB resident for the held streams"
    );
    assert!(all_answered.is_ok(), "not every SYN was answered");
    driver.abort();
    reader.abort();
    (accepted, resets)
}

#[tokio::test]
async fn a_guest_opening_streams_without_end_is_held_to_the_stream_cap() {
    let extra = 1000;
    let (accepted, resets) = flood(MAX_STREAMS as u32 + extra).await;
    assert_eq!(accepted, MAX_STREAMS);
    assert_eq!(resets, extra as usize, "each stream past the cap is reset");
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
    eprintln!(
        "one stream, a grant of nearly 4 GiB, a guest that reads nothing: {} MiB queued, \
         about {held_kib} KiB resident",
        queued >> 20
    );
    assert!(
        queued >= STREAM_WINDOW as usize,
        "the host queued only {queued} bytes: the window is cut too far"
    );
    assert!(
        queued <= (STREAM_WINDOW + INITIAL_STREAM_WINDOW) as usize,
        "the host queued {queued} bytes for a guest that reads none"
    );
    driver.abort();
}
