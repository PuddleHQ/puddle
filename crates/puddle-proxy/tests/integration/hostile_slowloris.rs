// SPDX-License-Identifier: GPL-3.0-or-later
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
//! then moves it on by the deadline.

use crate::hostile_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use hostile_support::{Guest, bystander_round, rig_with};
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
/// Wall-clock time the proxy gets to take every stream before the clock runs again by itself.
const TAKE_ALL_WITHIN: Duration = Duration::from_secs(60);

#[tokio::test]
async fn hostile_hg11_ten_thousand_stalled_heads_are_cut_off_at_the_deadline_and_over_the_cap_refused()
 {
    let refusals = Arc::new(Refusals::default());
    let _log = tracing::subscriber::set_default(
        tracing_subscriber::registry().with(CountRefusals(Arc::clone(&refusals))),
    );
    let config = ProxyConfig::default()
        .with_max_streams_per_workspace(CAP)
        .with_head_timeout(DEADLINE);
    let rig = rig_with(config).await;
    let mut guests = Vec::new();
    for _ in 0..SESSIONS {
        guests.push(Guest::connect(&rig.attacked).await);
    }
    // A paused clock still jumps to the next timer whenever the runtime waits on I/O, unless a
    // blocking task is running; this one runs until the test drops `thaw`.
    let (thaw, thawed) = std::sync::mpsc::channel::<()>();
    let mut frozen = tokio::task::spawn_blocking(move || {
        thawed.recv_timeout(TAKE_ALL_WITHIN) != Err(RecvTimeoutError::Timeout)
    });
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
            "the proxy refused {} of the {over_cap} streams over the cap within {TAKE_ALL_WITHIN:?}",
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
    assert!(
        answered
            .iter()
            .all(|took| *took >= DEADLINE && *took < Duration::from_secs(15)),
        "answered after {:?} to {:?}",
        answered.iter().min(),
        answered.iter().max()
    );
    assert!(
        bystander_took < Duration::from_secs(5),
        "the other workspace's 64 connections took {bystander_took:?}"
    );
}

/// How many streams the proxy has refused at the cap, counted from the warning it logs as it
/// refuses one.
#[derive(Default)]
struct Refusals {
    count: AtomicUsize,
    more: Notify,
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

struct CountRefusals(Arc<Refusals>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CountRefusals {
    fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, S>) {
        let mut message = Message(false);
        event.record(&mut message);
        if message.0 {
            self.0.count.fetch_add(1, Ordering::SeqCst);
            self.0.more.notify_waiters();
        }
    }
}

/// Whether an event's message is the proxy's refusal at the cap.
struct Message(bool);

impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 =
                format!("{value:?}") == "connection refused: workspace over its connection limit";
        }
    }
}
