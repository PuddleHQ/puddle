// SPDX-License-Identifier: AGPL-3.0-or-later
//! Decrypting every host: the switch, the hosts that are never decrypted, and the hosts a
//! workspace's tools turned out not to accept Puddle's certificate for.
//!
//! A workspace normally decrypts only the hosts it has a credential, secret or capture profile
//! for (its [`TerminationSet`](super::TerminationSet)). With the switch on it decrypts every
//! other host on port 443 too, except:
//!
//! - the names on its never-decrypt list (exact names and `*.` patterns), and
//! - a name its tools refused Puddle's certificate for within the last [`LEARNED_TTL`] (a tool
//!   with its own list of roots, or one that pins): the first connection of a tool fails, and the
//!   host is passed through from then on.
//!
//! A host with a credential binding is always decrypted: the credential has to be added.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use puddle_types::Host;

use super::TerminationSet;

/// How long a learned host stays passed through: Palo Alto's figure for its own exclusion cache.
pub const LEARNED_TTL: Duration = Duration::from_hours(12);

/// Most hosts remembered at once: Palo Alto's figure. Past it the oldest entry makes room.
pub const LEARNED_CAP: usize = 1024;

/// Called with a host the first time it is remembered (or again after it expired).
type Observer = Box<dyn Fn(&Host) + Send + Sync>;

/// Hosts a workspace's tools refused Puddle's certificate for, remembered for a while so that
/// those hosts are passed through. One per workspace; shared by every [`Termination`](super::Termination)
/// made for it, so a change to the workspace's settings does not forget what was learned.
pub struct PassThrough {
    inner: Mutex<Inner>,
    observer: Option<Observer>,
}

#[derive(Debug)]
struct Inner {
    ttl: Duration,
    cap: usize,
    seen: HashMap<String, Instant>,
}

impl std::fmt::Debug for PassThrough {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassThrough")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl Default for PassThrough {
    fn default() -> Self {
        Self::with_limits(LEARNED_TTL, LEARNED_CAP)
    }
}

impl PassThrough {
    /// An empty list with the default time and size.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty list that keeps a host for `ttl` and at most `cap` hosts at once.
    #[must_use]
    pub fn with_limits(ttl: Duration, cap: usize) -> Self {
        Self {
            inner: Mutex::new(Inner {
                ttl,
                cap: cap.max(1),
                seen: HashMap::new(),
            }),
            observer: None,
        }
    }

    /// Calls `observer` with each host that is newly remembered, so the owner can tell the user.
    /// It runs after the list's lock is released and must not block.
    #[must_use]
    pub fn observed(mut self, observer: impl Fn(&Host) + Send + Sync + 'static) -> Self {
        self.observer = Some(Box::new(observer));
        self
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Changes how long a host is kept and how many are kept. A list now over `cap` drops its
    /// oldest hosts; a shorter time applies to the hosts already in it.
    pub fn set_limits(&self, ttl: Duration, cap: usize) {
        let mut inner = self.inner();
        inner.ttl = ttl;
        inner.cap = cap.max(1);
        let now = Instant::now();
        inner.seen.retain(|_, at| now.duration_since(*at) < ttl);
        let excess = inner.seen.len().saturating_sub(inner.cap);
        if excess > 0 {
            let mut by_age: Vec<(Instant, String)> = inner
                .seen
                .iter()
                .map(|(name, at)| (*at, name.clone()))
                .collect();
            by_age.sort();
            for (_, name) in by_age.into_iter().take(excess) {
                inner.seen.remove(&name);
            }
        }
    }

    /// How long a host is kept.
    #[must_use]
    pub fn ttl(&self) -> Duration {
        self.inner().ttl
    }

    /// How many hosts are kept at most.
    #[must_use]
    pub fn cap(&self) -> usize {
        self.inner().cap
    }

    /// Remembers `host`, or renews it. Returns whether it was not already remembered.
    pub fn learn(&self, host: &Host) -> bool {
        let now = Instant::now();
        let fresh = {
            let mut inner = self.inner();
            let ttl = inner.ttl;
            inner.seen.retain(|_, at| now.duration_since(*at) < ttl);
            let key = host.to_string();
            if !inner.seen.contains_key(&key)
                && inner.seen.len() >= inner.cap
                && let Some(oldest) = inner
                    .seen
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(name, _)| name.clone())
            {
                inner.seen.remove(&oldest);
            }
            inner.seen.insert(key, now).is_none()
        };
        if fresh && let Some(observer) = &self.observer {
            observer(host);
        }
        fresh
    }

    /// Whether `host` is remembered and not yet expired.
    #[must_use]
    pub fn contains(&self, host: &Host) -> bool {
        let inner = self.inner();
        inner
            .seen
            .get(&host.to_string())
            .is_some_and(|at| at.elapsed() < inner.ttl)
    }

    /// Forgets `host`: it is decrypted again from the next connection. Returns whether it was
    /// remembered and not yet expired.
    pub fn forget(&self, host: &Host) -> bool {
        let mut inner = self.inner();
        let ttl = inner.ttl;
        inner
            .seen
            .remove(&host.to_string())
            .is_some_and(|at| at.elapsed() < ttl)
    }

    /// Forgets every host. Returns how many were remembered and not yet expired.
    pub fn clear(&self) -> usize {
        let mut inner = self.inner();
        let ttl = inner.ttl;
        let live = inner.seen.values().filter(|at| at.elapsed() < ttl).count();
        inner.seen.clear();
        live
    }

    /// Makes `host` look `by` older than it is (tests of the expiry, without waiting for it).
    #[cfg(test)]
    fn age(&self, host: &Host, by: Duration) {
        if let Some(at) = self.inner().seen.get_mut(&host.to_string()) {
            *at = at.checked_sub(by).unwrap_or(*at);
        }
    }

    /// The remembered hosts with the time each has left, soonest to expire first.
    #[must_use]
    pub fn list(&self) -> Vec<(String, Duration)> {
        let inner = self.inner();
        let mut out: Vec<(String, Duration)> = inner
            .seen
            .iter()
            .filter_map(|(name, at)| {
                inner
                    .ttl
                    .checked_sub(at.elapsed())
                    .filter(|left| !left.is_zero())
                    .map(|left| (name.clone(), left))
            })
            .collect();
        out.sort_by_key(|(_, left)| *left);
        out
    }
}

/// How one workspace decrypts beyond the hosts it has a credential for.
#[derive(Debug, Clone, Default)]
pub struct Interception {
    pub(crate) decrypt_all: bool,
    pub(crate) never: TerminationSet,
    pub(crate) curated: TerminationSet,
    pub(crate) learning: bool,
    pub(crate) learned: std::sync::Arc<PassThrough>,
}

impl Interception {
    /// Switched off: only the bound hosts are decrypted.
    #[must_use]
    pub fn new(learned: std::sync::Arc<PassThrough>) -> Self {
        Self {
            decrypt_all: false,
            never: TerminationSet::new(),
            curated: TerminationSet::new(),
            learning: true,
            learned,
        }
    }

    /// The hosts passed through without being tried, because tools are known to refuse
    /// Puddle's certificate for them (the built-in curated list, in the mode that has one).
    #[must_use]
    pub fn curated(mut self, curated: TerminationSet) -> Self {
        self.curated = curated;
        self
    }

    /// Whether a host whose tool refused Puddle's certificate is learned and passed through
    /// (on unless the user turned it off).
    #[must_use]
    pub fn learn_from_rejections(mut self, on: bool) -> Self {
        self.learning = on;
        self
    }

    /// Whether a refused certificate makes the host learned.
    #[must_use]
    pub fn is_learning(&self) -> bool {
        self.learning
    }

    /// Turns "decrypt every host" on or off.
    #[must_use]
    pub fn decrypt_all(mut self, on: bool) -> Self {
        self.decrypt_all = on;
        self
    }

    /// The names that are never decrypted while the switch is on.
    #[must_use]
    pub fn never_decrypt(mut self, never: TerminationSet) -> Self {
        self.never = never;
        self
    }

    /// Whether the switch is on.
    #[must_use]
    pub fn is_decrypting_all(&self) -> bool {
        self.decrypt_all
    }

    /// The names that are never decrypted.
    #[must_use]
    pub fn never(&self) -> &TerminationSet {
        &self.never
    }

    /// The hosts the workspace's tools did not accept the certificate for.
    #[must_use]
    pub fn learned(&self) -> &std::sync::Arc<PassThrough> {
        &self.learned
    }

    /// Why `host`, not bound to a credential, is not decrypted although the switch is on: the
    /// audit code. `None` when it is decrypted (or the switch is off).
    #[must_use]
    pub fn skipped(&self, host: &Host) -> Option<&'static str> {
        if !self.decrypt_all || !matches!(host, Host::Name(_)) {
            return None;
        }
        if self.never.contains(host) {
            Some("never_decrypt")
        } else if self.curated.contains(host) {
            Some("curated")
        } else if self.learned.contains(host) {
            Some("learned")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests;
