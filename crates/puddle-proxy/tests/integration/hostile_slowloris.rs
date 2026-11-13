// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ten thousand streams on one route, spread over a hundred agent sessions, each stop halfway
//! through a request head and stay silent (slowloris). The workspace's cap of 1000 holds: the first
//! 1000 are cut off at the head deadline with a `408` that names it, every other stream is refused
//! with a `503` that names the cap, and nothing waits for the default 30 s. A second workspace on
//! the same proxy finishes 64 connections in under 5 s meanwhile.
//!
//! Those counts hold only if every stream reaches the proxy before the first deadline, since a
//! slot a deadline frees goes to the next stream, which then gets a `408` of its own. A loaded
//! machine can take longer than the deadline just to open 10 000 streams (a CI run cut off 1489),
//! so tokio's clock stands still until the proxy has refused all 9000 over the cap, and the test
//! then moves it on by the deadline. That wait gives up only when the refusals stop coming for
//! [`STALL_LIMIT`], not after a fixed total time, since a machine slowed by other work takes as
//! long as it takes to refuse 9000 streams but keeps counting.

use crate::hostile_support;

use std::sync::Once;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use hostile_support::{Guest, assert_unharmed, bystander_round, rig_with};
use puddle_proxy::ProxyConfig;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;
use tokio::time::Instant;
use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, SubscriberExt};

const CAP: usize = 1000;
const SESSIONS: usize = 100;
const PER_SESSION: usize = 100;
const DEADLINE: Duration = Duration::from_secs(3);
/// Wall-clock time without one more refusal after which the proxy is taken to be stuck and the
/// clock runs again by itself. Progress resets it, so the time all 9000 take does not count.
const STALL_LIMIT: Duration = Duration::from_secs(30);
/// How often the frozen-clock task looks at the refusal count.
const STALL_POLL: Duration = Duration::from_millis(200);

#[tokio::test]
async fn hostile_hg11_ten_thousand_stalled_heads_are_cut_off_at_the_deadline_and_over_the_cap_refused()
 {
    count_refusals();
    let refusals = &REFUSALS;
    let config = ProxyConfig::default()
        .with_max_streams_per_workspace(CAP)
        .with_head_timeout(DEADLINE);
    let rig = rig_with(&config).await;
    let quiet = bystander_round(&rig.bystander, rig.echo).await;
    let mut guests = Vec::new();
    for _ in 0..SESSIONS {
        guests.push(Guest::connect(&rig.attacked).await);
    }
    // A paused clock still jumps to the next timer whenever the runtime waits on I/O, unless a
    // blocking task is running; this one runs until the test drops `thaw`.
    let (thaw, mut frozen) = hold_thread(refusals);
    tokio::time::pause();
    let started = Instant::now();
    let mut tasks = Vec::new();
    for guest in &mut guests {
        for _ in 0..PER_SESSION {
            let mut stream = guest.control.open_stream().await.unwrap();
            tasks.push(tokio::spawn(async move {
                stream
                    .write_all(b"CONNECT slow.test:443 HTTP/1.1\r\nX-Slow: ")
                    .await
                    .ok()?;
                let mut raw = Vec::new();
                tokio::time::timeout(Duration::from_secs(20), stream.read_to_end(&mut raw))
                    .await
                    .ok()?
                    .ok()?;
                Some((String::from_utf8_lossy(&raw).into_owned(), Instant::now()))
            }));
        }
    }
    let bystander_took = bystander_round(&rig.bystander, rig.echo).await;
    let over_cap = SESSIONS * PER_SESSION - CAP;
    tokio::select! {
        () = refusals.reach(over_cap) => {}
        _ = &mut frozen => panic!(
            "the proxy refused {} of the {over_cap} streams over the cap, then none for {STALL_LIMIT:?}",
            refusals.count.load(Ordering::SeqCst)
        ),
    }
    // tokio's timers fire on whole milliseconds, so a deadline falls up to 1 ms after it is due.
    tokio::time::advance(DEADLINE + Duration::from_millis(1)).await;
    let (mut cut_off, mut refused, mut other) = (0, 0, Vec::new());
    let mut answered = Vec::new();
    for task in tasks {
        match task.await.unwrap() {
            Some((text, at))
                if text.starts_with("HTTP/1.1 408") && text.contains("not received within 3s") =>
            {
                cut_off += 1;
                answered.push(at - started);
            }
            Some((text, at))
                if text.starts_with("HTTP/1.1 503")
                    && text
                        .contains("too many open connections from this workspace (limit 1000)") =>
            {
                refused += 1;
                answered.push(at - started);
            }
            answer => other.push(answer),
        }
    }
    drop(thaw);
    assert!(
        frozen.await.unwrap(),
        "the clock ran on by itself before every stream was answered"
    );
    assert_eq!(
        (cut_off, refused, other.len()),
        (CAP, over_cap, 0),
        "{:?}",
        other.iter().take(3).collect::<Vec<_>>()
    );
    // The deadline is what ended them: not at once, and nowhere near the 30 s default.
    // flake-ok: `took` is a tokio instant difference on the paused clock that `frozen` holds still,
    // so it is the 3 s `advance` above, not machine time (a stalled machine does not move it).
    assert!(
        answered
            .iter()
            .all(|took| *took >= DEADLINE && *took < Duration::from_secs(15)),
        "answered after {:?} to {:?}",
        answered.iter().min(),
        answered.iter().max()
    );
    assert_unharmed(bystander_took, quiet, "while the streams are held");
}

/// Starts a blocking task that runs until the returned sender is dropped, which keeps a paused
/// clock from jumping ahead. Its result is `true` when it was released and `false` when it gave
/// up after [`STALL_LIMIT`] without one more refusal.
fn hold_thread(
    watched: &'static Refusals,
) -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<bool>) {
    let (thaw, thawed) = std::sync::mpsc::channel::<()>();
    let held = tokio::task::spawn_blocking(move || {
        let (mut seen, mut since) = (0, std::time::Instant::now());
        loop {
            match thawed.recv_timeout(STALL_POLL) {
                Err(RecvTimeoutError::Timeout) => {}
                _ => return true,
            }
            let now = watched.count.load(Ordering::SeqCst);
            if now != seen {
                (seen, since) = (now, std::time::Instant::now());
            } else if since.elapsed() >= STALL_LIMIT {
                return false;
            }
        }
    });
    (thaw, held)
}

/// How many streams the proxy has refused at the cap, counted from the warning it logs as it
/// refuses one.
struct Refusals {
    count: AtomicUsize,
    more: Notify,
}

static REFUSALS: Refusals = Refusals {
    count: AtomicUsize::new(0),
    more: Notify::const_new(),
};

/// Installs the counter as the process's tracing subscriber, once. A subscriber set for this
/// thread only (`set_default`) is not enough: tracing caches per call site whether anyone listens,
/// and when another test thread of this binary reaches the refusal's call site first (the
/// `route` tests refuse at a cap of their own), that thread has no subscriber and the cache says
/// nobody listens, so this test would see none of its 9000 refusals and give up after
/// [`STALL_LIMIT`]. A global subscriber is found from every thread. Only refusals at this test's
/// cap count, so the other tests' refusals do not.
fn count_refusals() {
    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        // A failure means another subscriber is installed, and the test then fails by its stall
        // message with a count of 0.
        let _ = tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(CountRefusals),
        );
    });
}

impl Refusals {
    async fn reach(&self, target: usize) {
        loop {
            let more = self.more.notified();
            if self.count.load(Ordering::SeqCst) >= target {
                return;
            }
            more.await;
        }
    }
}

struct CountRefusals;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountRefusals {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut seen = Seen::default();
        event.record(&mut seen);
        if seen.is_refusal_at_cap() {
            REFUSALS.count.fetch_add(1, Ordering::SeqCst);
            REFUSALS.more.notify_waiters();
        }
    }
}

/// The message and the `limit` field of an event.
#[derive(Default)]
struct Seen {
    refusal: bool,
    limit: Option<u64>,
}

impl Seen {
    fn is_refusal_at_cap(&self) -> bool {
        self.refusal && self.limit == Some(CAP as u64)
    }
}

impl Visit for Seen {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.refusal =
                format!("{value:?}") == "connection refused: workspace over its connection limit";
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        if field.name() == "limit" {
            self.limit = Some(value);
        }
    }
}
