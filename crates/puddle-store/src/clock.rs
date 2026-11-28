// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host clock, in epoch milliseconds. The guest's clock plays no part (R-7).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// A source of the current time in epoch milliseconds.
pub trait Clock: Send + Sync {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&self) -> u64;

    /// Milliseconds on a clock that never steps back, from an arbitrary start. For measuring how
    /// long ago something was, never for a time a person reads. A clock with no such source
    /// reads the same as [`now_ms`](Self::now_ms).
    fn monotonic_ms(&self) -> u64 {
        self.now_ms()
    }
}

/// The host's wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        // A clock before 1970 reads as 0; one past u64 milliseconds (year 584 million) saturates.
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    fn monotonic_ms(&self) -> u64 {
        static START: OnceLock<Instant> = OnceLock::new();
        u64::try_from(START.get_or_init(Instant::now).elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// A clock that only moves when told to, for tests here and in other crates.
///
/// Its monotonic reading moves with every [`advance`](Self::advance) and every [`set`](Self::set)
/// forward, and stays put when `set` steps the time back, as the real one does.
#[derive(Debug, Default)]
pub struct ManualClock {
    wall: AtomicU64,
    monotonic: AtomicU64,
}

impl ManualClock {
    /// A clock reading `now_ms`.
    #[must_use]
    pub fn new(now_ms: u64) -> Self {
        Self {
            wall: AtomicU64::new(now_ms),
            monotonic: AtomicU64::new(now_ms),
        }
    }

    /// Sets the time.
    pub fn set(&self, now_ms: u64) {
        let before = self.wall.swap(now_ms, Ordering::SeqCst);
        self.monotonic
            .fetch_add(now_ms.saturating_sub(before), Ordering::SeqCst);
    }

    /// Moves the time forward by `ms`.
    pub fn advance(&self, ms: u64) {
        self.wall.fetch_add(ms, Ordering::SeqCst);
        self.monotonic.fetch_add(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.wall.load(Ordering::SeqCst)
    }

    fn monotonic_ms(&self) -> u64 {
        self.monotonic.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests;
