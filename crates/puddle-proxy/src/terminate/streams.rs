// SPDX-License-Identifier: AGPL-3.0-or-later
//! The open streams of one terminated HTTP/2 connection, and the clock that ends what stalls.
//!
//! A stream's idle timer cannot live in its body: the HTTP/2 library stops polling a body while it
//! waits for the peer to grant window, so a guest that grants none (or an upstream that never
//! sends) would pin the stream, its HTTP/1.1 lease and the upstream stream for as long as the
//! connection lives. Instead every open stream is registered here with the moment it last moved a
//! frame, and one [`Sweeper`] per connection looks at all of them on a slow tick:
//!
//! - a stream quiet for its limit is marked expired and its watchers are woken (a body that is
//!   being polled fails and the stream is reset);
//! - a stream still open a grace period after that was never polled, so the connection is asked
//!   to shut down gracefully;
//! - a connection that has not ended a grace period after the shutdown is dropped, which is the
//!   only thing a peer that never acknowledges the shutdown can be given.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::task::{Context as TaskContext, Poll, Waker};
use std::time::Duration;

use tokio::sync::OwnedSemaphorePermit;
use tokio::time::Instant;

/// How often a connection's streams are looked at.
pub(crate) const SWEEP_INTERVAL: Duration = Duration::from_secs(2);

/// The first limit that cut something on a connection, for the audit.
pub(crate) type LimitHit = Arc<OnceLock<&'static str>>;

/// Why a limit cut a stream or a connection (the audit's `limit:` reasons).
pub(crate) mod ended {
    /// A stream moved no frame for its limit and never gave way to a reset.
    pub(crate) const STREAM_IDLE: &str = "stream_idle";
    /// The peer did not take the graceful shutdown.
    pub(crate) const SHUTDOWN_IGNORED: &str = "shutdown_ignored";
    /// An agreed WebSocket pipe moved no byte for its limit.
    pub(crate) const WEBSOCKET_IDLE: &str = "websocket_idle";
}

/// When a stream last moved a frame in either direction, and whether it has been told to give up.
#[derive(Debug)]
pub(crate) struct Activity {
    base: Instant,
    last_ms: AtomicU64,
    /// How long the stream may be quiet.
    limit_ms: AtomicU64,
    state: Mutex<Expiry>,
    hit: LimitHit,
}

#[derive(Debug, Default)]
struct Expiry {
    expired_at: Option<Instant>,
    wakers: Vec<Waker>,
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl Activity {
    pub(crate) fn new(limit: Duration, hit: LimitHit) -> Self {
        Self {
            base: Instant::now(),
            last_ms: AtomicU64::new(0),
            limit_ms: AtomicU64::new(millis(limit)),
            state: Mutex::new(Expiry::default()),
            hit,
        }
    }

    /// Notes that `code` cut this stream (or its pipe), for the connection's audit record.
    pub(crate) fn note_limit(&self, code: &'static str) {
        let _ = self.hit.set(code);
    }

    pub(crate) fn touch(&self) {
        self.last_ms
            .store(millis(self.base.elapsed()), Ordering::Relaxed);
    }

    pub(crate) fn quiet_for(&self) -> Duration {
        let now = millis(self.base.elapsed());
        Duration::from_millis(now.saturating_sub(self.last_ms.load(Ordering::Relaxed)))
    }

    /// A pipe that was agreed after the stream began is quiet by design for longer or shorter
    /// than a request: it brings its own limit.
    pub(crate) fn set_limit(&self, limit: Duration) {
        self.limit_ms.store(millis(limit), Ordering::Relaxed);
    }

    fn limit(&self) -> Duration {
        Duration::from_millis(self.limit_ms.load(Ordering::Relaxed))
    }

    /// Tells the stream to give up and wakes whoever waits for that. Returns when it was first
    /// told.
    fn expire(&self, now: Instant) -> Instant {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let at = *state.expired_at.get_or_insert(now);
        for waker in state.wakers.drain(..) {
            waker.wake();
        }
        at
    }

    /// Ready once the stream has been told to give up; otherwise remembers `cx`'s waker.
    pub(crate) fn poll_expired(&self, cx: &mut TaskContext<'_>) -> Poll<()> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.expired_at.is_some() {
            return Poll::Ready(());
        }
        if !state.wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
            state.wakers.push(cx.waker().clone());
        }
        Poll::Pending
    }

    /// Completes once the stream has been told to give up.
    pub(crate) async fn expired(&self) {
        std::future::poll_fn(|cx| self.poll_expired(cx)).await;
    }

    fn expired_at(&self) -> Option<Instant> {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .expired_at
    }
}

/// The streams open on one guest connection.
#[derive(Debug, Default)]
pub(crate) struct Registry {
    hit: LimitHit,
    next: AtomicU64,
    open: Mutex<HashMap<u64, Arc<Activity>>>,
}

impl Registry {
    pub(crate) fn new(hit: LimitHit) -> Self {
        Self {
            hit,
            next: AtomicU64::new(0),
            open: Mutex::default(),
        }
    }

    /// Registers a new stream that may be quiet for `limit`, holding `permit` (its place in the
    /// workspace's stream budget) until the guard goes away.
    pub(crate) fn open(
        self: &Arc<Self>,
        limit: Duration,
        permit: Option<OwnedSemaphorePermit>,
    ) -> StreamGuard {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let activity = Arc::new(Activity::new(limit, Arc::clone(&self.hit)));
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, Arc::clone(&activity));
        StreamGuard {
            registry: Arc::clone(self),
            id,
            activity,
            _permit: permit,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    #[cfg(test)]
    pub(crate) fn sweep_for_test(&self, now: Instant) {
        self.sweep(now, Duration::from_secs(20));
    }

    /// Expires every stream that has been quiet for its limit. `true`: some stream was told to
    /// give up at least `grace` ago and is still open, so nothing polls it.
    fn sweep(&self, now: Instant, grace: Duration) -> bool {
        let open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let mut stuck = false;
        for activity in open.values() {
            if activity.quiet_for() >= activity.limit() {
                let since = activity.expire(now);
                stuck |= now.saturating_duration_since(since) >= grace;
            } else if let Some(since) = activity.expired_at() {
                // Moved again after being told to give up: the order stands.
                stuck |= now.saturating_duration_since(since) >= grace;
            }
        }
        stuck
    }
}

/// Counts a stream as open until the guard goes away (with the response body, so a stream that is
/// still sending is not mistaken for an idle connection), and holds its place in the workspace's
/// stream budget.
#[derive(Debug)]
pub(crate) struct StreamGuard {
    registry: Arc<Registry>,
    id: u64,
    activity: Arc<Activity>,
    _permit: Option<OwnedSemaphorePermit>,
}

impl StreamGuard {
    pub(crate) fn activity(&self) -> Arc<Activity> {
        Arc::clone(&self.activity)
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.registry
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// The limits one connection's sweeper enforces.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Clock {
    /// How long a connection with no stream may stay open.
    pub(crate) keepalive: Duration,
    /// How long after a shutdown (or an expired stream) the connection may take to end.
    pub(crate) grace: Duration,
}

/// What the connection should do after a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Nothing,
    /// Send the graceful shutdown (GOAWAY).
    Shutdown,
    /// Drop the connection; the reason is the audit's `limit:` code.
    Drop(&'static str),
}

/// The per-connection state of the sweep.
#[derive(Debug)]
pub(crate) struct Sweeper {
    clock: Clock,
    idle_since: Instant,
    shutdown_at: Option<Instant>,
    /// The shutdown was ordered because a stream would not give up.
    stuck: bool,
}

impl Sweeper {
    pub(crate) fn new(clock: Clock, now: Instant) -> Self {
        Self {
            clock,
            idle_since: now,
            shutdown_at: None,
            stuck: false,
        }
    }

    /// One look at the connection's streams at `now`.
    pub(crate) fn tick(&mut self, now: Instant, streams: &Registry) -> Step {
        let stuck = streams.sweep(now, self.clock.grace);
        let open = streams.len();
        if let Some(at) = self.shutdown_at {
            // A connection that is only waiting for its last streams is left alone; one that has
            // none, or whose streams will not move, has had its grace.
            let waited = now.saturating_duration_since(at) >= self.clock.grace;
            if waited && (open == 0 || self.stuck) {
                return Step::Drop(if self.stuck {
                    ended::STREAM_IDLE
                } else {
                    ended::SHUTDOWN_IGNORED
                });
            }
            return Step::Nothing;
        }
        if stuck {
            self.stuck = true;
            self.shutdown_at = Some(now);
            return Step::Shutdown;
        }
        if open > 0 {
            self.idle_since = now;
        } else if now.saturating_duration_since(self.idle_since) >= self.clock.keepalive {
            self.shutdown_at = Some(now);
            return Step::Shutdown;
        }
        Step::Nothing
    }
}

#[cfg(test)]
mod tests;
