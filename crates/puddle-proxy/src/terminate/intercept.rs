// SPDX-License-Identifier: GPL-3.0-or-later
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
        while inner.seen.len() > inner.cap {
            let Some(oldest) = inner
                .seen
                .iter()
                .min_by_key(|(_, at)| **at)
                .map(|(name, _)| name.clone())
            else {
                break;
            };
            inner.seen.remove(&oldest);
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
    pub(crate) learned: std::sync::Arc<PassThrough>,
}

impl Interception {
    /// Switched off: only the bound hosts are decrypted.
    #[must_use]
    pub fn new(learned: std::sync::Arc<PassThrough>) -> Self {
        Self {
            decrypt_all: false,
            never: TerminationSet::new(),
            learned,
        }
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
        } else if self.learned.contains(host) {
            Some("learned")
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn host(text: &str) -> Host {
        Host::parse_normalised(text).unwrap()
    }

    #[test]
    fn a_learned_host_is_remembered_until_its_time_is_up() {
        let list = PassThrough::with_limits(Duration::from_secs(3600), 8);
        assert!(list.learn(&host("pinned.example")));
        assert!(!list.learn(&host("pinned.example")), "renewing is not new");
        assert!(list.contains(&host("pinned.example")));
        assert!(!list.contains(&host("other.example")));
        assert_eq!(list.list().len(), 1);
        list.age(&host("pinned.example"), Duration::from_secs(3599));
        assert!(list.contains(&host("pinned.example")), "a second left");
        list.age(&host("pinned.example"), Duration::from_secs(2));
        assert!(!list.contains(&host("pinned.example")));
        assert_eq!(list.list(), []);
        assert!(
            list.learn(&host("pinned.example")),
            "new again after expiry"
        );
    }

    #[test]
    fn a_full_list_drops_its_oldest_host_and_a_forgotten_host_is_gone() {
        let list = PassThrough::with_limits(Duration::from_secs(60), 2);
        list.learn(&host("a.example"));
        list.age(&host("a.example"), Duration::from_secs(10));
        list.learn(&host("b.example"));
        list.age(&host("b.example"), Duration::from_secs(5));
        list.learn(&host("c.example"));
        assert!(!list.contains(&host("a.example")));
        assert!(list.contains(&host("b.example")) && list.contains(&host("c.example")));
        list.forget(&host("b.example"));
        assert!(!list.contains(&host("b.example")));
        assert_eq!(list.list().len(), 1);
    }

    #[test]
    fn forgetting_says_whether_the_host_was_remembered_and_clearing_counts_the_live_ones() {
        let list = PassThrough::with_limits(Duration::from_secs(60), 8);
        list.learn(&host("a.example"));
        list.learn(&host("b.example"));
        list.learn(&host("c.example"));
        list.age(&host("c.example"), Duration::from_secs(61));
        assert!(list.forget(&host("a.example")));
        assert!(!list.forget(&host("a.example")), "already gone");
        assert!(
            !list.forget(&host("c.example")),
            "expired is not remembered"
        );
        assert_eq!(list.clear(), 1, "only b.example was still live");
        assert_eq!(list.list(), []);
        assert_eq!(list.clear(), 0);
    }

    #[test]
    fn new_limits_apply_to_the_hosts_already_remembered() {
        let list = PassThrough::with_limits(Duration::from_secs(3600), 8);
        for (name, age) in [("a", 30), ("b", 20), ("c", 10)] {
            let h = host(&format!("{name}.example"));
            list.learn(&h);
            list.age(&h, Duration::from_secs(age));
        }
        list.set_limits(Duration::from_secs(3600), 2);
        assert!(!list.contains(&host("a.example")), "the oldest made room");
        assert_eq!(list.list().len(), 2);
        list.set_limits(Duration::from_secs(15), 0);
        assert_eq!(list.cap(), 1, "a cap of zero is one");
        assert_eq!(list.ttl(), Duration::from_secs(15));
        assert_eq!(
            list.list()
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["c.example"],
            "a shorter time expired b at once"
        );
    }

    #[test]
    fn the_observer_hears_each_new_host_once() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&heard);
        let list = PassThrough::new().observed(move |h| sink.lock().unwrap().push(h.to_string()));
        list.learn(&host("a.example"));
        list.learn(&host("a.example"));
        list.learn(&host("b.example"));
        assert_eq!(*heard.lock().unwrap(), ["a.example", "b.example"]);
        assert!(format!("{list:?}").contains("PassThrough"));
    }

    #[test]
    fn the_defaults_are_twelve_hours_and_a_thousand_hosts() {
        let list = PassThrough::new();
        assert_eq!(list.ttl(), Duration::from_hours(12));
        assert_eq!(list.cap(), 1024);
        assert_eq!(PassThrough::with_limits(Duration::ZERO, 0).cap(), 1);
    }

    #[test]
    fn only_a_decrypting_workspace_skips_hosts_and_it_says_why() {
        let learned = Arc::new(PassThrough::new());
        learned.learn(&host("pinned.example"));
        let never = TerminationSet::parse(["bank.example", "*.health.example"]).unwrap();
        let off = Interception::new(Arc::clone(&learned)).never_decrypt(never.clone());
        assert!(!off.is_decrypting_all());
        assert_eq!(off.skipped(&host("bank.example")), None, "switch off");
        let on = off.clone().decrypt_all(true);
        assert!(on.is_decrypting_all() && on.never().contains(&host("bank.example")));
        assert_eq!(on.skipped(&host("bank.example")), Some("never_decrypt"));
        assert_eq!(on.skipped(&host("a.health.example")), Some("never_decrypt"));
        assert_eq!(on.skipped(&host("pinned.example")), Some("learned"));
        assert_eq!(on.skipped(&host("registry.npmjs.org")), None);
        assert_eq!(
            on.skipped(&host("140.82.112.3")),
            None,
            "an IP is never decrypted"
        );
        assert!(Arc::ptr_eq(on.learned(), &learned));
    }
}
