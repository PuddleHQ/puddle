// SPDX-License-Identifier: GPL-3.0-or-later
//! A guest holds its workspace at the connection cap and then opens 100 000 more streams on its
//! route. Every one is refused at the cap with the reason in the answer, the proxy's heap stays
//! within a fixed bar, a second workspace on the same proxy finishes 64 connections in under 5 s
//! while the flood runs, and the capped workspace works again once its held streams end.
//! This is the only test in the binary, so the allocator's count is the proxy's and the guests'.

mod hostile_heap;
mod hostile_support;

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream;
use hostile_heap::Counting;
use hostile_support::{Guest, bystander_round, rig_with};
use puddle_proxy::ProxyConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[global_allocator]
static HEAP: Counting = Counting::new();

const CAP: usize = 1000;
const FLOOD: usize = 100_000;
/// Streams in flight at once. With all 100 000 at once about a tenth got no answer within 10 s
/// (the proxy answers each over-the-cap stream in turn), which would say nothing about the cap.
const AT_ONCE: usize = 2000;
/// What the heap may rise by while 100 000 streams are refused: about 10 MB measured, so a
/// refusal that kept its buffers (100 000 x 8 KiB alone is 800 MB) cannot pass.
const HEAP_BAR: usize = 64 * 1024 * 1024;

/// One flooding stream: `true` when the answer is the cap's refusal naming its limit.
async fn refused_at_the_cap(guest: &Guest) -> Option<bool> {
    let mut control = guest.control.clone();
    let mut stream = control.open_stream().await.ok()?;
    stream
        .write_all(b"CONNECT blocked.test:443 HTTP/1.1\r\n\r\n")
        .await
        .ok()?;
    let mut raw = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut raw))
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&raw);
    Some(
        text.starts_with("HTTP/1.1 503")
            && text.contains("too many open connections from this workspace (limit 1000)"),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_hg11_a_hundred_thousand_streams_over_the_cap_are_each_refused_and_the_heap_is_bounded()
 {
    let config = ProxyConfig::default()
        .with_max_streams_per_workspace(CAP)
        .with_head_timeout(Duration::from_secs(120));
    let rig = rig_with(config).await;
    let mut holder = Guest::connect(&rig.attacked).await;
    // The cap: CAP streams that stop halfway through their head are held until the deadline.
    let mut held = Vec::new();
    for _ in 0..CAP {
        let mut stream = holder.control.open_stream().await.unwrap();
        stream
            .write_all(b"CONNECT slow.test:443 HTTP/1.1\r\nX: ")
            .await
            .unwrap();
        held.push(stream);
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let before = HEAP.restart_peak();
    let started = Instant::now();
    let flooder = Guest::connect(&rig.attacked).await;
    let flooding = async {
        let mut refused = 0_usize;
        let mut other = 0_usize;
        let mut unanswered = 0_usize;
        let mut results = stream::iter(0..FLOOD)
            .map(|_| refused_at_the_cap(&flooder))
            .buffer_unordered(AT_ONCE);
        while let Some(result) = results.next().await {
            match result {
                Some(true) => refused += 1,
                Some(false) => other += 1,
                None => unanswered += 1,
            }
        }
        (refused, other, unanswered)
    };
    let bystander = async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        bystander_round(&rig.bystander, rig.echo).await
    };
    let ((refused, other, unanswered), bystander_took) = tokio::join!(flooding, bystander);
    let elapsed = started.elapsed();
    let rose = HEAP.rose_above(before);
    assert_eq!(
        (refused, other, unanswered),
        (FLOOD, 0, 0),
        "every stream over the cap is answered 503 with the cap's reason"
    );
    assert!(
        rose < HEAP_BAR,
        "the heap rose by {rose} bytes (bar {HEAP_BAR})"
    );
    assert!(
        bystander_took < Duration::from_secs(5),
        "the other workspace's 64 connections took {bystander_took:?} during the flood ({elapsed:?})"
    );
    // The held streams were never answered (their heads are unfinished) and still hold the cap; once
    // they end the workspace connects again.
    drop(held);
    drop(holder);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut guest = Guest::connect(&rig.attacked).await;
    loop {
        let (code, _) = guest.connect_status(rig.echo).await;
        if code == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the workspace never connected again after its held streams ended (last answer {code})"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
