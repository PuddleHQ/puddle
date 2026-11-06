// SPDX-License-Identifier: GPL-3.0-or-later
//! An in-memory cache with expiry, single flight and a stale grace. Nothing is written to disk.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::time::Instant;

use crate::error::SourceError;
use crate::readlog::{ReadLog, ReadUse};
use crate::sources::{Credential, Fetch};
use crate::spec::SourceSpec;

/// How long a value of one source kind is trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ttl {
    /// The value is served as is for this long; `None` means until it is invalidated.
    pub fresh: Option<Duration>,
    /// After `fresh`, a failed refresh keeps serving the old value this much longer.
    pub grace: Duration,
    /// A value nobody asked for this long is dropped from memory and read again on the next ask;
    /// `None` means it is kept while it is otherwise valid.
    pub idle: Option<Duration>,
}

/// The expiry per source kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ttls {
    /// `gh`: 55 minutes, like the Docker sandboxes refresh.
    pub gh: Ttl,
    /// Git credential helpers: 30 minutes.
    pub git_credential: Ttl,
    /// Pasted tokens: until the user changes or removes them, but dropped from memory after 30
    /// minutes without a use (the next use reads the operating system's store again).
    pub stored: Ttl,
    /// The Azure CLI: 55 minutes at most, and earlier when the token says it ends sooner.
    pub az_cli: Ttl,
}

/// How long a pasted token stays in memory without being used.
const STORED_IDLE: Duration = Duration::from_mins(30);

impl Default for Ttls {
    fn default() -> Self {
        let grace = Duration::from_mins(5);
        Self {
            gh: Ttl {
                fresh: Some(Duration::from_mins(55)),
                grace,
                idle: None,
            },
            git_credential: Ttl {
                fresh: Some(Duration::from_mins(30)),
                grace,
                idle: None,
            },
            stored: Ttl {
                fresh: None,
                grace: Duration::ZERO,
                idle: Some(STORED_IDLE),
            },
            az_cli: Ttl {
                fresh: Some(Duration::from_mins(55)),
                grace,
                idle: None,
            },
        }
    }
}

impl Ttls {
    fn for_spec(&self, spec: &SourceSpec) -> Ttl {
        match spec {
            SourceSpec::Gh { .. } => self.gh,
            SourceSpec::GitCredential { .. } => self.git_credential,
            SourceSpec::Stored { .. } => self.stored,
            SourceSpec::AzCli { .. } => self.az_cli,
        }
    }
}

/// A source needs the user to sign in. The proxy's 502 and the UI's "Sign in" notice come from
/// this; it names the source and nothing secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInNeeded {
    /// The source that could not supply its secret.
    pub source: SourceSpec,
}

struct Held {
    credential: Credential,
    /// Past this the value is refreshed; `None` = never.
    refresh_at: Option<Instant>,
    /// Past this the old value is never served again; `None` = never.
    dead_at: Option<Instant>,
    /// Past this without a use the value is dropped; `None` = never. Each use moves it.
    idle_at: Option<Instant>,
    /// How far each use moves `idle_at`.
    idle: Option<Duration>,
}

impl Held {
    /// Nothing may serve this any more: its grace is over, or nobody used it for too long.
    fn gone(&self, now: Instant) -> bool {
        self.dead_at.is_some_and(|at| now >= at) || self.idle_at.is_some_and(|at| now >= at)
    }
}

type Slot = Arc<tokio::sync::Mutex<Option<Held>>>;

/// Caches what a [`Fetch`] returns. One lookup per source at a time (a burst of requests is one
/// spawn); a refresh that fails keeps the old value until its grace ends, then fails.
pub struct SecretCache<F> {
    fetcher: F,
    ttls: Ttls,
    slots: Mutex<HashMap<SourceSpec, Slot>>,
    events: broadcast::Sender<SignInNeeded>,
    log: Option<Arc<ReadLog>>,
}

/// A credential should be renewed this long before its source says it ends.
const RENEW_EARLY: Duration = Duration::from_mins(5);

impl<F: Fetch + Send + Sync + 'static> SecretCache<F> {
    /// Runs [`evict_expired`](Self::evict_expired) every minute until the last `Arc` of the cache
    /// is gone. Needs a Tokio runtime.
    pub fn spawn_sweeper(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(SWEEP_EVERY).await;
                let Some(cache) = weak.upgrade() else {
                    return;
                };
                cache.evict_expired();
            }
        })
    }
}

/// How often the sweeper looks for values to drop.
const SWEEP_EVERY: Duration = Duration::from_mins(1);

impl<F: Fetch> SecretCache<F> {
    /// A cache over `fetcher` with the default expiry.
    pub fn new(fetcher: F) -> Self {
        Self::with_ttls(fetcher, Ttls::default())
    }

    /// A cache over `fetcher` with `ttls`.
    pub fn with_ttls(fetcher: F, ttls: Ttls) -> Self {
        let (events, _) = broadcast::channel(16);
        Self {
            fetcher,
            ttls,
            slots: Mutex::new(HashMap::new()),
            events,
            log: None,
        }
    }

    /// The same, noting the outcome of every read the source itself answers (not one served from
    /// memory) in `log`.
    #[must_use]
    pub fn with_read_log(mut self, log: Arc<ReadLog>) -> Self {
        self.log = Some(log);
        self
    }

    /// Sign-in notices, one per failed read that needs the user.
    pub fn subscribe(&self) -> broadcast::Receiver<SignInNeeded> {
        self.events.subscribe()
    }

    /// Drops what is held for `spec` (the user changed or removed it, or signed in).
    pub fn invalidate(&self, spec: &SourceSpec) {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(spec);
    }

    /// Drops every held value that may not be served any more (its grace is over, or nobody used
    /// it for its idle time), so an idle source does not keep its plaintext until the process
    /// ends. A value being read right now is left alone. Returns how many were dropped.
    pub fn evict_expired(&self) -> usize {
        let now = Instant::now();
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let mut dropped = 0;
        slots.retain(|_, slot| {
            let Ok(mut held) = slot.try_lock() else {
                return true;
            };
            if held.as_ref().is_some_and(|h| h.gone(now)) {
                *held = None;
                dropped += 1;
            }
            held.is_some() || Arc::strong_count(slot) > 1
        });
        dropped
    }

    /// The credential `spec` names: from the cache while fresh, else read again.
    ///
    /// # Errors
    /// The source's [`SourceError`] when it cannot supply a value and no earlier one is still
    /// allowed.
    pub async fn get(&self, spec: &SourceSpec) -> Result<Credential, SourceError> {
        self.get_for(spec, ReadUse::Injection).await
    }

    /// [`get`](Self::get) for a read that `by` asked for, which the read log records.
    ///
    /// # Errors
    /// As [`get`](Self::get).
    pub async fn get_for(&self, spec: &SourceSpec, by: ReadUse) -> Result<Credential, SourceError> {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(slots.entry(spec.clone()).or_default())
        };
        let mut held = slot.lock().await;
        let now = Instant::now();
        if held
            .as_ref()
            .is_some_and(|h| h.idle_at.is_some_and(|at| now >= at))
        {
            *held = None;
        }
        if let Some(h) = held.as_mut()
            && h.refresh_at.is_none_or(|at| now < at)
        {
            h.idle_at = h.idle.map(|d| now + d);
            return Ok(h.credential.clone());
        }
        match self.fetcher.fetch(spec).await {
            Ok(fetched) => {
                if let Some(log) = &self.log {
                    log.record(spec, by, None);
                }
                let ttl = self.ttls.for_spec(spec);
                let mut refresh_at = ttl.fresh.map(|d| now + d);
                let mut dead_at = refresh_at.map(|at| at + ttl.grace);
                if let Some(valid) = fetched.valid_for {
                    let renew = now + valid.saturating_sub(RENEW_EARLY);
                    refresh_at = Some(refresh_at.map_or(renew, |at| at.min(renew)));
                    let end = now + valid;
                    dead_at = Some(dead_at.map_or(end, |at| at.min(end)));
                }
                let credential = fetched.credential.clone();
                *held = Some(Held {
                    credential: fetched.credential,
                    refresh_at,
                    dead_at,
                    idle_at: ttl.idle.map(|d| now + d),
                    idle: ttl.idle,
                });
                Ok(credential)
            }
            Err(err) => {
                if let Some(log) = &self.log {
                    log.record(spec, by, Some(err.clone()));
                }
                if let Some(h) = held.as_ref()
                    && h.dead_at.is_none_or(|at| now < at)
                {
                    tracing::warn!(source = %spec.describe(), "refresh failed, serving the earlier value");
                    return Ok(h.credential.clone());
                }
                *held = None;
                if err.needs_sign_in() {
                    let _ = self.events.send(SignInNeeded {
                        source: spec.clone(),
                    });
                }
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::name::{AccountName, HostName, StoredId};
    use crate::secret::Secret;
    use crate::sources::Fetched;
    use crate::spec::TokenScope;

    /// Answers from a script, one entry per call, and counts the calls.
    struct Script {
        answers: Mutex<VecDeque<Result<Option<Duration>, SourceError>>>,
        calls: AtomicUsize,
    }

    impl Script {
        fn new(answers: Vec<Result<Option<Duration>, SourceError>>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                calls: AtomicUsize::new(0),
            }
        }
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Fetch for &Script {
        fn fetch(
            &self,
            _: &SourceSpec,
        ) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let next = self
                .answers
                .lock()
                .unwrap()
                .pop_front()
                .expect("scripted answer");
            std::future::ready(next.map(|valid_for| Fetched {
                credential: Credential {
                    username: None,
                    secret: Arc::new(Secret::new(format!("v{n}"))),
                },
                valid_for,
            }))
        }
    }

    fn stored() -> SourceSpec {
        SourceSpec::Stored {
            id: StoredId::new("a").unwrap(),
            scope: TokenScope {
                host: HostName::new("github.com").unwrap(),
                org: None,
            },
        }
    }

    fn gh() -> SourceSpec {
        SourceSpec::Gh {
            host: HostName::new("github.com").unwrap(),
            account: AccountName::new("me").unwrap(),
        }
    }

    const MIN: Duration = Duration::from_secs(60);

    async fn value(cache: &SecretCache<&Script>, spec: &SourceSpec) -> Result<String, SourceError> {
        cache.get(spec).await.map(|c| c.secret.expose().to_owned())
    }

    #[tokio::test(start_paused = true)]
    async fn serves_from_memory_until_the_ttl_then_reads_again() {
        let script = Script::new(vec![Ok(None), Ok(None)]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        tokio::time::advance(54 * MIN).await;
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        assert_eq!(script.calls(), 1);
        tokio::time::advance(2 * MIN).await;
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v1");
        assert_eq!(script.calls(), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_refresh_serves_the_old_value_until_the_grace_ends() {
        let script = Script::new(vec![
            Ok(None),
            Err(SourceError::Timeout(crate::Tool::Gh)),
            Err(SourceError::NotSignedIn),
        ]);
        let cache = SecretCache::new(&script);
        let mut events = cache.subscribe();
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        tokio::time::advance(56 * MIN).await;
        assert_eq!(
            value(&cache, &gh()).await.unwrap(),
            "v0",
            "refresh failed, still in grace"
        );
        // flake-ok: the cache sends the notice with a synchronous `broadcast::send` before `value` returns,
        // and `value` was awaited, so a notice would already be in the channel.
        assert!(
            events.try_recv().is_err(),
            "no notice while an old value still works"
        );
        tokio::time::advance(5 * MIN).await;
        assert_eq!(
            value(&cache, &gh()).await.unwrap_err(),
            SourceError::NotSignedIn
        );
        // flake-ok: same as above: the send happened before `value` returned.
        assert_eq!(events.try_recv().unwrap().source, gh());
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_that_expires_early_is_renewed_early() {
        let script = Script::new(vec![Ok(Some(20 * MIN)), Ok(Some(20 * MIN))]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        tokio::time::advance(14 * MIN).await;
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        tokio::time::advance(2 * MIN).await;
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v1");
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_past_its_end_is_never_served_after_a_failed_refresh() {
        let script = Script::new(vec![
            Ok(Some(10 * MIN)),
            Err(SourceError::Timeout(crate::Tool::Git)),
        ]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        tokio::time::advance(11 * MIN).await;
        assert!(value(&cache, &gh()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_used_stored_value_never_expires_and_invalidate_drops_it() {
        let spec = stored();
        let script = Script::new(vec![Ok(None), Ok(None)]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &spec).await.unwrap(), "v0");
        for _ in 0..100 {
            tokio::time::advance(20 * MIN).await;
            assert_eq!(value(&cache, &spec).await.unwrap(), "v0");
        }
        assert_eq!(script.calls(), 1);
        cache.invalidate(&spec);
        assert_eq!(value(&cache, &spec).await.unwrap(), "v1");
    }

    #[tokio::test(start_paused = true)]
    async fn az_cli_renews_five_minutes_before_expiry() {
        let az = SourceSpec::AzCli {
            tenant: crate::name::TenantId::new("t1").unwrap(),
            org: None,
        };
        // A token that ends in 20 minutes is renewed at minute 15, long before the 55 minute limit.
        let script = Script::new(vec![
            Ok(Some(20 * MIN)),
            Ok(Some(90 * MIN)),
            Ok(Some(90 * MIN)),
        ]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &az).await.unwrap(), "v0");
        tokio::time::advance(14 * MIN).await;
        assert_eq!(value(&cache, &az).await.unwrap(), "v0");
        tokio::time::advance(2 * MIN).await;
        assert_eq!(value(&cache, &az).await.unwrap(), "v1");
        // A 90 minute token is still renewed after 55 minutes.
        tokio::time::advance(54 * MIN).await;
        assert_eq!(value(&cache, &az).await.unwrap(), "v1");
        tokio::time::advance(2 * MIN).await;
        assert_eq!(value(&cache, &az).await.unwrap(), "v2");
    }

    #[tokio::test(start_paused = true)]
    async fn sources_are_cached_separately() {
        let other = SourceSpec::Gh {
            host: HostName::new("github.com").unwrap(),
            account: AccountName::new("you").unwrap(),
        };
        let script = Script::new(vec![Ok(None), Ok(None)]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        assert_eq!(value(&cache, &other).await.unwrap(), "v1");
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
    }

    #[test]
    fn each_kind_of_source_has_its_own_time_to_live() {
        let ttls = Ttls::default();
        let host = HostName::new("github.com").unwrap();
        let gh = SourceSpec::Gh {
            host: host.clone(),
            account: AccountName::new("me").unwrap(),
        };
        let git = SourceSpec::GitCredential {
            host: host.clone(),
            path: crate::name::UrlPath::new("acme").unwrap(),
            username: None,
        };
        let stored = SourceSpec::Stored {
            id: StoredId::new("t1").unwrap(),
            scope: TokenScope { host, org: None },
        };
        assert_eq!(ttls.for_spec(&gh), ttls.gh);
        assert_eq!(ttls.for_spec(&git), ttls.git_credential);
        assert_eq!(ttls.for_spec(&stored), ttls.stored);
        assert_ne!(ttls.gh, ttls.git_credential);
    }

    #[tokio::test(start_paused = true)]
    async fn the_read_log_notes_each_read_the_source_answers_and_never_a_cached_one() {
        let script = Script::new(vec![Ok(None), Err(SourceError::NotSignedIn)]);
        let log = Arc::new(ReadLog::new());
        let cache = SecretCache::new(&script).with_read_log(Arc::clone(&log));
        assert_eq!(log.last(&gh()), None);
        cache.get_for(&gh(), ReadUse::RepositoryList).await.unwrap();
        let first = log.last(&gh()).unwrap();
        assert_eq!((first.by, &first.problem), (ReadUse::RepositoryList, &None));
        // Served from memory: no read, so the entry is the same one.
        tokio::time::advance(MIN).await;
        cache.get(&gh()).await.unwrap();
        assert_eq!(log.last(&gh()), Some(first));
        // The TTL ends and the source now says no, past the grace.
        tokio::time::advance(61 * MIN).await;
        assert!(cache.get(&gh()).await.is_err());
        let last = log.last(&gh()).unwrap();
        assert_eq!(
            (last.by, last.problem),
            (ReadUse::Injection, Some(SourceError::NotSignedIn))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_value_is_read_again_at_the_very_moment_its_time_to_live_ends() {
        let script = Script::new(vec![Ok(None), Ok(None)]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        tokio::time::advance(Duration::from_millis(55 * 60 * 1000 - 1)).await;
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v1");
    }

    #[tokio::test(start_paused = true)]
    async fn the_old_value_is_not_served_at_the_very_moment_its_grace_ends() {
        let script = Script::new(vec![Ok(None), Err(SourceError::Timeout(crate::Tool::Gh))]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        // 55 minutes of life and 5 of grace end together at 60.
        tokio::time::advance(60 * MIN).await;
        assert_eq!(
            value(&cache, &gh()).await.unwrap_err(),
            SourceError::Timeout(crate::Tool::Gh)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_token_is_served_after_a_failed_refresh_until_its_own_end() {
        let script = Script::new(vec![
            Ok(Some(20 * MIN)),
            Err(SourceError::Timeout(crate::Tool::Gh)),
            Err(SourceError::Timeout(crate::Tool::Gh)),
        ]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        // Past the early renewal (15 minutes) and before the end (20): the old token still works.
        tokio::time::advance(16 * MIN).await;
        assert_eq!(value(&cache, &gh()).await.unwrap(), "v0");
        // At the end it does not, though the cache's own time to live would allow it.
        tokio::time::advance(4 * MIN).await;
        assert!(value(&cache, &gh()).await.is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stored_value_nobody_asks_for_is_read_again_after_the_idle_time() {
        let script = Script::new(vec![Ok(None), Ok(None)]);
        let cache = SecretCache::new(&script);
        assert_eq!(value(&cache, &stored()).await.unwrap(), "v0");
        tokio::time::advance(31 * MIN).await;
        assert_eq!(value(&cache, &stored()).await.unwrap(), "v1");
    }

    #[tokio::test(start_paused = true)]
    async fn evicting_drops_the_secret_of_an_idle_source_without_a_new_ask() {
        let script = Script::new(vec![Ok(None), Ok(None)]);
        let cache = SecretCache::new(&script);
        let weak = Arc::downgrade(&cache.get(&stored()).await.unwrap().secret);
        let gh_weak = Arc::downgrade(&cache.get(&gh()).await.unwrap().secret);
        tokio::time::advance(20 * MIN).await;
        assert_eq!(cache.evict_expired(), 0, "nothing is over yet");
        tokio::time::advance(11 * MIN).await;
        assert_eq!(cache.evict_expired(), 1, "the pasted token was idle");
        assert!(weak.upgrade().is_none(), "the buffer is released");
        assert!(gh_weak.upgrade().is_some(), "gh is still inside its time");
        tokio::time::advance(30 * MIN).await;
        assert_eq!(cache.evict_expired(), 1, "gh is past its grace");
        assert!(gh_weak.upgrade().is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn the_sweeper_evicts_on_its_own_and_stops_with_the_cache() {
        let script: &'static Script = Box::leak(Box::new(Script::new(vec![Ok(None)])));
        let cache = Arc::new(SecretCache::new(script));
        let handle = cache.spawn_sweeper();
        let weak = Arc::downgrade(&cache.get(&stored()).await.unwrap().secret);
        // The sweeper sleeps one minute at a time, so let it run between the steps.
        for _ in 0..33 {
            tokio::time::advance(MIN).await;
            tokio::task::yield_now().await;
        }
        assert!(weak.upgrade().is_none());
        drop(cache);
        tokio::time::advance(2 * MIN).await;
        handle.await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_value_being_read_right_now_is_left_alone_by_the_sweep() {
        let script = Script::new(vec![Ok(None)]);
        let cache = SecretCache::new(&script);
        cache.get(&stored()).await.unwrap();
        tokio::time::advance(40 * MIN).await;
        let slot = Arc::clone(cache.slots.lock().unwrap().get(&stored()).unwrap());
        let reading = slot.lock().await;
        assert_eq!(cache.evict_expired(), 0, "a slot in use is skipped");
        drop(reading);
        assert_eq!(cache.evict_expired(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn the_sweep_forgets_an_emptied_slot_nobody_waits_on() {
        let script = Script::new(vec![Ok(None)]);
        let cache = SecretCache::new(&script);
        cache.get(&stored()).await.unwrap();
        tokio::time::advance(40 * MIN).await;
        // Someone holds the slot, as a read about to start would.
        let waiting = Arc::clone(cache.slots.lock().unwrap().get(&stored()).unwrap());
        assert_eq!(cache.evict_expired(), 1);
        assert_eq!(cache.slots.lock().unwrap().len(), 1, "kept for the waiter");
        drop(waiting);
        assert_eq!(cache.evict_expired(), 0);
        assert!(cache.slots.lock().unwrap().is_empty(), "nobody waits: gone");
    }
}
