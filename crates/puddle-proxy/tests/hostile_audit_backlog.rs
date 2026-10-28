// SPDX-License-Identifier: GPL-3.0-or-later
//! A stream over the workspace's connection limit is answered first and audited after. While the
//! connection log lags (here it is held shut), the refusals waiting for their audit row must not
//! keep their streams: a flood would otherwise hold a task and a read buffer for every refused
//! stream for as long as the log lags, and the cap would bound nothing. This is the only test in
//! the binary, and the guest's streams are closed before the heap is read, so the allocator's count
//! is what the proxy holds.

#[expect(dead_code, reason = "each test binary uses a different subset")]
mod hostile_heap;
mod hostile_support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::{StreamExt, stream};
use hostile_heap::Counting;
use hostile_support::{Guest, rig_logged};
use puddle_proxy::ProxyConfig;
use puddle_types::{ConnectionEvent, ConnectionLog, ConnectionReason};

#[global_allocator]
static HEAP: Counting = Counting::new();

const REFUSED: usize = 300;
const AT_ONCE: usize = 100;
/// What the proxy may hold for each refusal waiting for its audit row: its task and the blocking
/// thread waiting on the log, about 15 KB measured. A refusal that kept its stream adds the 8 KiB
/// read buffer (about 23 KB), so the bar sits between the two.
const PER_WAITING: usize = 18 * 1024;

/// A connection log that can be shut: while shut it writes nothing, and every write waits until
/// it opens again. It counts the cap's refusals it writes.
struct HeldLog {
    open: Mutex<bool>,
    opened: Condvar,
    refusals: AtomicUsize,
}

impl HeldLog {
    fn new() -> Self {
        Self {
            open: Mutex::new(true),
            opened: Condvar::new(),
            refusals: AtomicUsize::new(0),
        }
    }

    fn set_open(&self, open: bool) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = open;
        self.opened.notify_all();
    }

    fn refusals(&self) -> usize {
        self.refusals.load(Ordering::Relaxed)
    }
}

impl ConnectionLog for HeldLog {
    fn record(&self, event: &ConnectionEvent) {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        while !*open {
            open = self
                .opened
                .wait(open)
                .unwrap_or_else(PoisonError::into_inner);
        }
        if event.reason == ConnectionReason::ConnectionLimit {
            self.refusals.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Opens the log when dropped: a failed assertion must not leave it shut, because the runtime
/// waits for the threads writing to it.
struct Reopen(Arc<HeldLog>);

impl Drop for Reopen {
    fn drop(&mut self) {
        self.0.set_open(true);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hostile_hg11_refusals_waiting_for_their_audit_row_hold_no_stream() {
    let log = Arc::new(HeldLog::new());
    let _reopen = Reopen(log.clone());
    let config = ProxyConfig::default().with_max_streams_per_workspace(1);
    let rig = rig_logged(&config, log.clone()).await;
    let mut guest = Guest::connect(&rig.attacked).await;
    let (code, _held) = guest.connect_status(rig.echo).await;
    assert_eq!(code, 200, "the workspace's one slot is taken");
    log.set_open(false);

    let before = HEAP.live();
    let answers: Vec<_> = stream::iter(0..REFUSED)
        .map(|_| guest.refused_at_the_cap(1))
        .buffer_unordered(AT_ONCE)
        .collect()
        .await;
    assert_eq!(
        answers.iter().filter(|a| **a == Some(true)).count(),
        REFUSED,
        "every stream over the cap is answered 503 with the cap's reason while the log is shut"
    );
    assert_eq!(
        log.refusals(),
        0,
        "no refusal was audited while the log was shut"
    );
    // The guest's streams are closed; the proxy's side of them ends a moment later.
    let bar = REFUSED * PER_WAITING;
    let settle = Instant::now() + Duration::from_secs(5);
    let mut waiting = HEAP.live().saturating_sub(before);
    while waiting >= bar && Instant::now() < settle {
        tokio::time::sleep(Duration::from_millis(50)).await;
        waiting = HEAP.live().saturating_sub(before);
    }
    assert!(
        waiting < bar,
        "{REFUSED} answered refusals waiting for their audit rows hold {waiting} bytes (bar {bar})"
    );

    log.set_open(true);
    let deadline = Instant::now() + Duration::from_secs(20);
    while log.refusals() < REFUSED {
        assert!(
            Instant::now() < deadline,
            "only {} of {REFUSED} refusals reached the audit once the log opened",
            log.refusals()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    eprintln!("{REFUSED} refusals waiting for the audit held {waiting} bytes");
}
