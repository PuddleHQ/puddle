// SPDX-License-Identifier: AGPL-3.0-or-later
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
                    // no listener means nobody to prompt; the error below still reaches the caller
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
mod tests;
