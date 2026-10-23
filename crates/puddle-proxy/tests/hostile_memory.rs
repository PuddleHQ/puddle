// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy's memory does not follow what a hostile guest sends: twelve guests at once push a
//! 10 MB request line or 100 000 header lines at one route, and the proxy's heap (counted by this
//! test binary's allocator) stays far below what buffering one of the heads would take. The guests
//! run in a child process, as a real guest runs in its own machine: the bytes a guest has queued
//! but not yet sent are its memory, not the proxy's. This is the only test in the binary, so
//! nothing else allocates meanwhile.

mod hostile_heap;
mod hostile_support;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use hostile_heap::Counting;
use hostile_support::{Guest, Guests, bystander_round, guest_route, rig};

#[global_allocator]
static HEAP: Counting = Counting::new();

const TEST: &str = "hostile_hg10_the_heap_does_not_follow_what_a_guest_sends";
/// What the child prints once every guest is sending.
const SENDING: &str = "guests: sending";
const ATTACKERS: usize = 12;
/// Buffering one 10 MB line would already pass this.
const LIMIT: usize = 12 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_hg10_the_heap_does_not_follow_what_a_guest_sends() {
    if let Some(route) = guest_route() {
        guests(&route).await;
        // skip-ok: the child's half; the parent process that started it makes the assertions.
        return;
    }
    let rig = rig().await;
    // The counter sees what it must: holding 16 MiB moves the peak by at least that much.
    let before = HEAP.restart_peak();
    let held = vec![1_u8; 16 * 1024 * 1024];
    assert!(HEAP.rose_above(before) >= held.len());
    drop(held);

    let before = HEAP.restart_peak();
    let mut guests = Guests::start(TEST, &rig.attacked);
    guests.until(SENDING).await;
    let timed = bystander_round(&rig.bystander, rig.echo).await;
    assert!(timed < Duration::from_secs(5), "{timed:?}");
    let (ok, said) = guests.finish(Duration::from_secs(60)).await;
    let grew = HEAP.rose_above(before);
    assert!(
        grew < LIMIT,
        "the proxy's heap grew by {grew} bytes (limit {LIMIT}); the guests said: {said}"
    );
    assert!(ok, "guests failed: {said}");
    assert_eq!(
        rig.policy.decisions(),
        64,
        "only the bystander reached the rules"
    );
    eprintln!("the proxy's heap grew by {grew} bytes; the guests said: {said}");
}

/// The child's part: twelve guests on the route at `path` each send one oversized head, every one
/// is refused with a 431 naming the limit, and the child's own heap rise is reported.
#[expect(
    clippy::unwrap_used,
    clippy::print_stdout,
    reason = "the child's half of the test: it reports to the parent on stdout and fails by panicking"
)]
async fn guests(path: &Path) {
    let ten_mb_line: Arc<[u8]> = [b"GET http://a.test/".to_vec(), vec![b'a'; 10 * 1024 * 1024]]
        .concat()
        .into();
    let many_headers: Arc<[u8]> = format!(
        "GET http://a.test/ HTTP/1.1\r\n{}\r\n",
        "X-A: b\r\n".repeat(100_000)
    )
    .into_bytes()
    .into();
    let mut guests = Vec::new();
    for _ in 0..ATTACKERS {
        guests.push(Guest::connect_at(path).await);
    }
    let before = HEAP.restart_peak();
    let mut attacks = Vec::new();
    for (n, mut guest) in guests.into_iter().enumerate() {
        let bytes = Arc::clone(if n % 2 == 0 {
            &ten_mb_line
        } else {
            &many_headers
        });
        attacks.push(tokio::spawn(async move { guest.refused(bytes).await }));
    }
    println!("{SENDING}");
    for attack in attacks {
        let (code, body) = attack.await.unwrap();
        assert_eq!(
            (code, body.contains("request head over 64 KiB")),
            (431, true),
            "{body:?}"
        );
    }
    let sent = ten_mb_line.len() * (ATTACKERS / 2) + many_headers.len() * (ATTACKERS / 2);
    println!(
        "guests: {sent} bytes sent, the guests' own heap rose by {} bytes",
        HEAP.rose_above(before)
    );
}
