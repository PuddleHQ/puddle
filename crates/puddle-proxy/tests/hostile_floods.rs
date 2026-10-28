// SPDX-License-Identifier: GPL-3.0-or-later
//! A guest holds its workspace at the connection cap and then opens 100 000 more streams on its
//! route. Every one is refused at the cap with the reason in the answer, the proxy's heap stays
//! within a fixed bar, a second workspace on the same proxy finishes 64 connections in under 5 s
//! while the flood runs, and the capped workspace works again once its held streams end. The
//! flooding guest runs in a child process, as a real guest runs in its own machine: its streams
//! and the frames it has queued but not yet sent are its memory, not the proxy's. This is the only
//! test in the binary, so the allocator's count in the parent is the proxy's.

mod hostile_heap;
mod hostile_support;

use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream;
use hostile_heap::Counting;
use hostile_support::{Guest, Guests, bystander_round, guest_route, rig_with};
use puddle_proxy::ProxyConfig;
use tokio::io::AsyncWriteExt;

#[global_allocator]
static HEAP: Counting = Counting::new();

const TEST: &str =
    "hostile_hg11_a_hundred_thousand_streams_over_the_cap_are_each_refused_and_the_heap_is_bounded";
/// What the child prints once it holds the cap and starts the flood.
const FLOODING: &str = "guests: flooding";
/// What the child prints once every flooding stream has its answer.
const FLOODED: &str = "guests: flooded";
const CAP: usize = 1000;
const FLOOD: usize = 100_000;
/// Streams in flight at once. With all 100 000 at once about a tenth got no answer within 10 s
/// (the proxy answers each over-the-cap stream in turn), which would say nothing about the cap.
const AT_ONCE: usize = 2000;
/// What the proxy's heap may rise by while 100 000 streams are refused: a few MB measured, so a
/// refusal that kept its buffers (100 000 x 8 KiB alone is 800 MB) cannot pass.
const HEAP_BAR: usize = 64 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_hg11_a_hundred_thousand_streams_over_the_cap_are_each_refused_and_the_heap_is_bounded()
 {
    if let Some(route) = guest_route() {
        guests(&route).await;
        // skip-ok: the child's half; the parent process that started it makes the assertions.
        return;
    }
    let config = ProxyConfig::default()
        .with_max_streams_per_workspace(CAP)
        .with_head_timeout(Duration::from_secs(120));
    let rig = rig_with(&config).await;
    let mut guests = Guests::start(TEST, &rig.attacked);
    guests.until(FLOODING).await;
    let before = HEAP.restart_peak();
    let started = Instant::now();
    let bystander = async {
        // The test's timeline: the bystander starts once the flood is under way.
        tokio::time::sleep(Duration::from_millis(500)).await;
        bystander_round(&rig.bystander, rig.echo).await
    };
    let ((), bystander_took) = tokio::join!(guests.until(FLOODED), bystander);
    let elapsed = started.elapsed();
    let rose = HEAP.rose_above(before);
    let (ok, said) = guests.finish(Duration::from_secs(60)).await;
    assert!(ok, "guests failed: {said}");
    assert!(
        rose < HEAP_BAR,
        "the proxy's heap rose by {rose} bytes (bar {HEAP_BAR}); the guests said: {said}"
    );
    assert!(
        bystander_took < Duration::from_secs(5),
        "the other workspace's 64 connections took {bystander_took:?} during the flood ({elapsed:?})"
    );
    eprintln!("the proxy's heap rose by {rose} bytes in {elapsed:?}; the guests said: {said}");
    // The held streams were never answered (their heads are unfinished) and held the cap until the
    // child ended; now the workspace connects again.
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

/// The child's part: CAP streams that stop halfway through their head hold the workspace at its
/// cap, then `FLOOD` more streams are opened, `AT_ONCE` at a time, and every one must be refused with
/// the cap's reason. The child's own heap rise is reported.
#[expect(
    clippy::unwrap_used,
    clippy::print_stdout,
    reason = "the child's half of the test: it reports to the parent on stdout and fails by panicking"
)]
async fn guests(path: &Path) {
    let mut holder = Guest::connect_at(path).await;
    let mut held = Vec::new();
    for _ in 0..CAP {
        let mut stream = holder.control.open_stream().await.unwrap();
        stream
            .write_all(b"CONNECT slow.test:443 HTTP/1.1\r\nX: ")
            .await
            .unwrap();
        held.push(stream);
    }
    // Settle window: the proxy has accepted the flood connections before the timing starts.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let flooder = Guest::connect_at(path).await;
    let before = HEAP.restart_peak();
    println!("{FLOODING}");
    let mut refused = 0_usize;
    let mut other = 0_usize;
    let mut unanswered = 0_usize;
    let mut results = stream::iter(0..FLOOD)
        .map(|_| flooder.refused_at_the_cap(CAP))
        .buffer_unordered(AT_ONCE);
    while let Some(result) = results.next().await {
        match result {
            Some(true) => refused += 1,
            Some(false) => other += 1,
            None => unanswered += 1,
        }
    }
    assert_eq!(
        (refused, other, unanswered),
        (FLOOD, 0, 0),
        "every stream over the cap is answered 503 with the cap's reason"
    );
    println!(
        "{FLOODED}: {FLOOD} refused, the guests' own heap rose by {} bytes",
        HEAP.rose_above(before)
    );
}
