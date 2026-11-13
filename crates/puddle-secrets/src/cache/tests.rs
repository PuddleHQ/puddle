// SPDX-License-Identifier: AGPL-3.0-or-later
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
    fn fetch(&self, _: &SourceSpec) -> impl Future<Output = Result<Fetched, SourceError>> + Send {
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
