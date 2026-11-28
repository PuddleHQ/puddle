// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a hostile guest can make the host's yamux session hold: streams that cost a task each, and
//! window grants that let the host queue bytes the guest never reads. Each test reports what it
//! measured in the failure message, so the cost per stream and per grant is on record.
#![expect(clippy::unwrap_used, reason = "a failed step fails the test")]

use std::time::Duration;

use futures_util::StreamExt;
use puddle_agent_proto::yamux::{MAX_STREAMS, SEND_BUDGET, STREAM_WINDOW, server_config};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};
use tokio_yamux::{Config, Session, config::INITIAL_STREAM_WINDOW};

const WINDOW_UPDATE: u8 = 1;
const SYN: u16 = 1;
const ACK: u16 = 2;
const RST: u16 = 8;
const DATA: u8 = 0;

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

/// A guest opens `streams` streams, grants each nearly 4 GiB (cut to one window by the host) and
/// reads nothing. One host task per stream writes 1 MiB at a time until its write waits. Returns
/// the bytes the host's sessions accepted from its streams in total, and the resident KiB.
async fn queued_for_a_guest_that_reads_nothing(config: Config, streams: u32) -> (usize, u64) {
    let (peer, ours) = duplex(1 << 16);
    let mut session = Session::new_server(ours, config);
    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel();
    let driver = tokio::spawn(async move {
        while let Some(Ok(stream)) = session.next().await {
            stream_tx.send(stream).unwrap();
        }
    });
    let (_rx, mut tx) = tokio::io::split(peer);
    for n in 0..streams {
        let id = 2 * n + 1;
        tx.write_all(&header(WINDOW_UPDATE, SYN, id, 0))
            .await
            .unwrap();
        tx.write_all(&header(
            WINDOW_UPDATE,
            0,
            id,
            u32::MAX - INITIAL_STREAM_WINDOW,
        ))
        .await
        .unwrap();
    }
    let before = rss_kib();
    let mut writers = Vec::new();
    for _ in 0..streams {
        let mut stream = stream_rx.recv().await.unwrap();
        writers.push(tokio::spawn(async move {
            let chunk = vec![0u8; 1 << 20];
            let mut queued = 0usize;
            while let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_millis(500), stream.write(&chunk)).await
            {
                queued += n;
            }
            // The stream stays open: a closed one would hand its window back.
            (queued, stream)
        }));
    }
    let mut total = 0;
    let mut held = Vec::new();
    for writer in writers {
        let (queued, stream) = writer.await.unwrap();
        total += queued;
        held.push(stream);
    }
    let held_kib = rss_kib().saturating_sub(before);
    driver.abort();
    (total, held_kib)
}

#[tokio::test]
async fn a_guest_that_reads_nothing_cannot_make_a_session_queue_more_than_its_send_budget() {
    let streams = 4;
    // Measured before the budget: the same guest against a session without one.
    let unbounded = Config {
        max_send_budget: usize::MAX,
        ..server_config()
    };
    let (before, before_kib) = queued_for_a_guest_that_reads_nothing(unbounded, streams).await;
    assert!(
        before >= streams as usize * STREAM_WINDOW as usize,
        "without a budget the host queued only {before} bytes: the hostile guest measures nothing"
    );
    let (after, after_kib) = queued_for_a_guest_that_reads_nothing(server_config(), streams).await;
    // The duplex pipe takes 64 KiB and the sink a little more before the budget is felt.
    let slack = 1 << 20;
    assert!(
        after <= SEND_BUDGET + slack,
        "with the budget the host queued {after} bytes ({after_kib} KiB resident), \
         without it {before} ({before_kib} KiB resident)"
    );
    assert!(
        after + slack >= SEND_BUDGET,
        "the budget is cut too far: {after} of {SEND_BUDGET} bytes queued"
    );
}

#[tokio::test]
async fn a_reading_guest_gets_more_than_the_send_budget_through_one_session() {
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
    let (mut rx, mut tx) = tokio::io::split(peer);
    tx.write_all(&header(WINDOW_UPDATE, SYN, 1, 0))
        .await
        .unwrap();
    let mut stream = stream_rx.await.unwrap();
    // The guest reads every frame and grants the window back, as a well-behaved peer does.
    let reader = tokio::spawn(async move {
        let mut h = [0u8; 12];
        let mut body = vec![0u8; 1 << 20];
        let mut read = 0usize;
        loop {
            rx.read_exact(&mut h).await.unwrap();
            let length = u32::from_be_bytes([h[8], h[9], h[10], h[11]]);
            if h[1] != DATA {
                continue;
            }
            let mut left = length as usize;
            while left > 0 {
                let n = rx.read(&mut body[..left.min(1 << 20)]).await.unwrap();
                left -= n;
            }
            read += length as usize;
            tx.write_all(&header(WINDOW_UPDATE, 0, 1, length))
                .await
                .unwrap();
            if read >= 3 * SEND_BUDGET {
                return read;
            }
        }
    });
    let chunk = vec![0u8; 1 << 20];
    let writer = tokio::spawn(async move {
        loop {
            if stream.write_all(&chunk).await.is_err() {
                break;
            }
        }
    });
    let read = tokio::time::timeout(Duration::from_secs(30), reader)
        .await
        .unwrap()
        .unwrap();
    assert!(read >= 3 * SEND_BUDGET, "only {read} bytes got through");
    writer.abort();
    driver.abort();
}
