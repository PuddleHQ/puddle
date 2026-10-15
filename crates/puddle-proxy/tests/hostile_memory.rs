// SPDX-License-Identifier: GPL-3.0-or-later
//! The proxy's memory does not follow what a hostile guest sends: twelve guests at once push a
//! 10 MB request line or 100 000 header lines at one route, and the heap (counted by this test
//! binary's allocator, the guests' own buffers included) stays far below what buffering one of the
//! heads would take. This is the only test in the binary, so nothing else allocates meanwhile.

mod hostile_support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use std::sync::Arc;
use std::time::Duration;

use hostile_support::{Guest, bystander_round, rig};

/// Live heap bytes and the highest the count has been.
struct Counting {
    live: AtomicUsize,
    peak: AtomicUsize,
}

#[global_allocator]
static HEAP: Counting = Counting {
    live: AtomicUsize::new(0),
    peak: AtomicUsize::new(0),
};

// SAFETY: every call is forwarded to `System`, which upholds the `GlobalAlloc` contract; the counters
// only observe the sizes.
#[expect(unsafe_code, reason = "a counting allocator wraps the system one")]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller's contract for `alloc` is `System`'s.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = self.live.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            self.peak.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.live.fetch_sub(layout.size(), Ordering::Relaxed);
        // SAFETY: the caller's contract for `dealloc` is `System`'s.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller's contract for `realloc` is `System`'s.
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            let now = if new_size >= layout.size() {
                self.live
                    .fetch_add(new_size - layout.size(), Ordering::Relaxed)
                    + (new_size - layout.size())
            } else {
                self.live
                    .fetch_sub(layout.size() - new_size, Ordering::Relaxed)
                    - (layout.size() - new_size)
            };
            self.peak.fetch_max(now, Ordering::Relaxed);
        }
        new
    }
}

const ATTACKERS: usize = 12;
/// Buffering one 10 MB line would already pass this.
const LIMIT: usize = 12 * 1024 * 1024;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_hg10_the_heap_does_not_follow_what_a_guest_sends() {
    let rig = rig().await;
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
        guests.push(Guest::connect(&rig.attacked).await);
    }
    // The attack bytes (shared, not copied per guest) and the connections exist; what is measured
    // is what sending them costs.
    let before = HEAP.live.load(Ordering::Relaxed);
    HEAP.peak.store(before, Ordering::Relaxed);
    // The counter sees what it must: holding 16 MiB moves the peak by at least that much.
    let held = vec![1_u8; 16 * 1024 * 1024];
    assert!(HEAP.peak.load(Ordering::Relaxed) - before >= held.len());
    drop(held);
    let before = HEAP.live.load(Ordering::Relaxed);
    HEAP.peak.store(before, Ordering::Relaxed);
    let mut attacks = Vec::new();
    for (n, mut guest) in guests.into_iter().enumerate() {
        let bytes = Arc::clone(if n % 2 == 0 {
            &ten_mb_line
        } else {
            &many_headers
        });
        attacks.push(tokio::spawn(async move { guest.refused(bytes).await }));
    }
    let timed = bystander_round(&rig.bystander, rig.echo).await;
    assert!(timed < Duration::from_secs(5), "{timed:?}");
    for attack in attacks {
        let (code, body) = attack.await.unwrap();
        assert_eq!(
            (code, body.contains("request head over 64 KiB")),
            (431, true),
            "{body:?}"
        );
    }
    assert_eq!(
        rig.policy.decisions(),
        64,
        "only the bystander reached the rules"
    );
    let grew = HEAP.peak.load(Ordering::Relaxed).saturating_sub(before);
    let sent = ten_mb_line.len() * (ATTACKERS / 2) + many_headers.len() * (ATTACKERS / 2);
    assert!(
        grew < LIMIT,
        "the heap grew by {grew} bytes (limit {LIMIT}) while {sent} bytes were sent"
    );
}
