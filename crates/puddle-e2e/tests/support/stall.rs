// SPDX-License-Identifier: GPL-3.0-or-later
//! Tells a stall of Puddle's own from the kernel backing off after it dropped a loopback segment.
//!
//! A Linux kernel from 6.16 on (the runners' 6.17) can drop a segment that sits inside the
//! receiver's window when the receive queue is full, and the sender then waits out its
//! retransmission timer, doubling it on each repeat (0.2, 0.4, 0.8, 1.6, 3.2 s). A paused reader
//! is what flow control is, so Puddle cannot prevent it; a bar on "no gap over 5 s" has to leave
//! that wait out and keep every pause Puddle causes. The two look different in `/proc/net/tcp`: a
//! sender in retransmission backoff has its retransmit timer armed (`tr` 1) and a nonzero
//! `retrnsmt`, while a sender facing a closed window runs the zero-window probe timer (`tr` 4)
//! with `retrnsmt` 0, so a reader that stops reading is never forgiven.
//!
//! Linux only: elsewhere nothing is sampled, nothing is subtracted, and the bar is the raw gap.
#![allow(dead_code, reason = "each test binary uses some of the helpers")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const SAMPLE_EVERY: Duration = Duration::from_millis(20);

/// One socket of `/proc/net/tcp` that touches a watched port.
pub(crate) struct Sample {
    key: (u16, u16),
    /// The timer kind (`tr`): 1 is the retransmit timer, 4 the zero-window probe timer.
    timer: u8,
    /// Jiffies left on that timer (`tm->when`).
    when: u64,
    /// Timeout retransmissions since the last new ACK (`retrnsmt`).
    retransmits: u32,
}

/// The sockets of `table` (the text of `/proc/net/tcp`) whose local or remote port is in `ports`.
pub(crate) fn parse(table: &str, ports: &[u16]) -> Vec<Sample> {
    table
        .lines()
        .skip(1)
        .filter_map(|row| {
            let f: Vec<&str> = row.split_whitespace().collect();
            let port = |i: usize| {
                f.get(i)
                    .and_then(|a| a.rsplit(':').next())
                    .and_then(|p| u16::from_str_radix(p, 16).ok())
            };
            let (local, remote) = (port(1)?, port(2)?);
            if !ports.contains(&local) && !ports.contains(&remote) {
                return None;
            }
            let (timer, when) = f.get(5)?.split_once(':')?;
            Some(Sample {
                key: (local, remote),
                timer: u8::from_str_radix(timer, 16).ok()?,
                when: u64::from_str_radix(when, 16).ok()?,
                retransmits: f.get(6)?.parse().ok()?,
            })
        })
        .collect()
}

/// What one socket was doing at its last sample.
struct Track {
    /// Since when its retransmit timer has been counting down without being re-armed: the last
    /// ACK that moved it. A timeout retransmission is the end of that wait, so the wait counts as
    /// backoff once the retransmission shows.
    countdown_since: Instant,
    last_when: u64,
    last_seen: Instant,
    backoff: bool,
}

/// Adds up the time some watched socket was in retransmission backoff: its retransmit timer
/// armed (`tr` 1) and `retrnsmt` above 0, and the wait that led to its first timeout. A closed
/// window is not backoff: that sender runs the probe timer (`tr` 4) with `retrnsmt` 0.
#[derive(Default)]
pub(crate) struct Accrual {
    socks: std::collections::HashMap<(u16, u16), Track>,
    credited_until: Option<Instant>,
    total: Duration,
}

impl Accrual {
    /// Takes one sample of the table, at `now`. Time is counted once however many sockets are in
    /// backoff at the same moment.
    pub(crate) fn observe(&mut self, now: Instant, samples: &[Sample]) {
        let mut since: Option<Instant> = None;
        for s in samples {
            let armed = s.timer == 1;
            let track = self.socks.entry(s.key).or_insert(Track {
                countdown_since: now,
                last_when: s.when,
                last_seen: now,
                backoff: false,
            });
            let backoff = armed && s.retransmits > 0;
            if !backoff && (!armed || s.when > track.last_when) {
                track.countdown_since = now;
            }
            if backoff {
                let from = if track.backoff {
                    track.last_seen
                } else {
                    track.countdown_since
                };
                since = Some(since.map_or(from, |other| other.min(from)));
            }
            track.backoff = backoff;
            track.last_seen = now;
            track.last_when = s.when;
        }
        if let Some(from) = since {
            let from = self.credited_until.map_or(from, |done| done.max(from));
            self.total += now.saturating_duration_since(from);
            self.credited_until = Some(now);
        }
    }

    pub(crate) fn total(&self) -> Duration {
        self.total
    }
}

/// Samples `/proc/net/tcp` for sockets on the given ports, 20 ms apart, until dropped.
///
/// It samples on its own thread, never on the runtime: the kernel builds that file from every TCP
/// socket on the host, so with tens of thousands open (a busy test suite) one read takes 50 to
/// 80 ms, and a sampler on the runtime would hold a worker that long, and back to back once its
/// interval fell behind, starving the very transfer it measures.
pub(crate) struct Backoff {
    nanos: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Backoff {
    pub(crate) fn watch(ports: Vec<u16>) -> Self {
        let nanos = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopped) = (Arc::clone(&nanos), Arc::clone(&stop));
        let thread = std::thread::spawn(move || {
            let mut accrual = Accrual::default();
            while !stopped.load(Ordering::Relaxed) {
                let Ok(table) = std::fs::read_to_string("/proc/net/tcp") else {
                    return;
                };
                accrual.observe(Instant::now(), &parse(&table, &ports));
                shared.store(
                    u64::try_from(accrual.total().as_nanos()).unwrap_or(u64::MAX),
                    Ordering::Relaxed,
                );
                std::thread::sleep(SAMPLE_EVERY);
            }
        });
        Self {
            nanos,
            stop,
            thread: Some(thread),
        }
    }

    /// The time seen in backoff since the watch began.
    pub(crate) fn accrued(&self) -> Duration {
        Duration::from_nanos(self.nanos.load(Ordering::Relaxed))
    }
}

impl Drop for Backoff {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// One gap between two pieces of progress.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Gap {
    /// Wall-clock time since the last progress.
    pub(crate) raw: Duration,
    /// Of that, the time a watched sender sat in retransmission backoff.
    pub(crate) kernel: Duration,
}

impl Gap {
    /// What no one but the transfer's own parties can have caused: the raw gap less the kernel's
    /// backoff. This is what the bar is on.
    pub(crate) fn own(&self) -> Duration {
        self.raw.saturating_sub(self.kernel)
    }
}

/// Measures the gaps of one transfer.
pub(crate) struct Progress<'a> {
    backoff: &'a Backoff,
    last: Instant,
    kernel_at_last: Duration,
    /// The gap with the largest own time.
    pub(crate) worst: Gap,
    /// The gap with the largest wall-clock time, whatever part of it was the kernel's. It is the
    /// one a planted drop makes; `worst` can be another gap, a few hundred milliseconds of
    /// scheduling on a loaded host, once the drop's own time is subtracted.
    pub(crate) longest: Gap,
}

impl<'a> Progress<'a> {
    pub(crate) fn new(backoff: &'a Backoff) -> Self {
        Self {
            backoff,
            last: Instant::now(),
            kernel_at_last: backoff.accrued(),
            worst: Gap::default(),
            longest: Gap::default(),
        }
    }

    /// The gap so far, without ending it.
    pub(crate) fn gap(&self) -> Gap {
        Gap {
            raw: self.last.elapsed(),
            kernel: self.backoff.accrued().saturating_sub(self.kernel_at_last),
        }
    }

    /// Progress happened: closes the gap and starts the next.
    pub(crate) fn mark(&mut self) {
        let gap = self.gap();
        if gap.own() >= self.worst.own() {
            self.worst = gap;
        }
        if gap.raw >= self.longest.raw {
            self.longest = gap;
        }
        self.last = Instant::now();
        self.kernel_at_last = self.backoff.accrued();
    }
}

/// The kernel's count of segments dropped for a full receive queue (`TcpExt: TCPRcvQDrop`), 0 when
/// it cannot be read.
pub(crate) fn rcvq_drops() -> u64 {
    let Ok(text) = std::fs::read_to_string("/proc/net/netstat") else {
        return 0;
    };
    let mut lines = text.lines();
    while let (Some(names), Some(values)) = (lines.next(), lines.next()) {
        if names.starts_with("TcpExt:") {
            let at = names.split_whitespace().position(|n| n == "TCPRcvQDrop");
            return at
                .and_then(|i| values.split_whitespace().nth(i))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
        }
    }
    0
}
