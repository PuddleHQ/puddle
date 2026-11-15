// SPDX-License-Identifier: AGPL-3.0-or-later
//! The proxy-script signal: the script answers `DIRECT` for a name while it names proxies for
//! other URLs, which is the script author's own definition of "internal".
//!
//! Evaluating a script can take seconds, and the guard that asks is not allowed to wait, so the
//! first question about a name starts an evaluation in the background and answers "no"; the
//! answer is kept for later questions. A name the guest retries is judged right the second time.
//! Failures are kept for a short while, so a network without a script costs one lookup a minute.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use puddle_upstream::{Destination, Hop, OsProxy, PacQuery, Scheme};

/// A URL the script is asked about to learn whether it names proxies at all: a public site no
/// company script exempts by name.
const REFERENCE_HOST: &str = "example.com";
/// Evaluations that may run at once.
const MAX_PENDING: usize = 4;
/// Names whose answer is kept at most.
const MAX_NAMES: usize = 1024;

/// How long answers are kept and how long one evaluation may take.
#[derive(Debug, Clone, Copy)]
pub struct PacTiming {
    /// How long an answer is kept.
    pub ttl: Duration,
    /// How long a failed evaluation is kept, so it is not repeated for every request.
    pub failure_ttl: Duration,
    /// Longest one evaluation may take.
    pub timeout: Duration,
}

impl Default for PacTiming {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(300),
            failure_ttl: Duration::from_secs(60),
            timeout: Duration::from_secs(5),
        }
    }
}

/// Runs a background job.
pub(super) type Spawn = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

#[derive(Default)]
struct Cache {
    /// Name to (when the answer expires, whether the script says `DIRECT`).
    answers: HashMap<String, (Instant, bool)>,
    /// Names being evaluated.
    pending: HashSet<String>,
    /// When the answer to "does the script name proxies" expires, and the answer.
    names_proxies: Option<(Instant, bool)>,
}

/// Asks the system's proxy script about names, in the background.
pub struct PacReader {
    os: Arc<dyn OsProxy>,
    timing: PacTiming,
    spawn: Spawn,
    cache: Arc<Mutex<Cache>>,
}

impl PacReader {
    /// A reader over `os` that evaluates on a thread of its own.
    #[must_use]
    pub fn new(os: Arc<dyn OsProxy>, timing: PacTiming) -> Self {
        Self::with_spawn(
            os,
            timing,
            Arc::new(|job| {
                // A thread that cannot start drops the job, which releases its pending mark.
                let _ = std::thread::Builder::new()
                    .name("puddle-pac-signal".to_owned())
                    .spawn(job);
            }),
        )
    }

    /// A reader that hands its evaluations to `spawn` (tests run them in place).
    #[must_use]
    pub fn with_spawn(os: Arc<dyn OsProxy>, timing: PacTiming, spawn: Spawn) -> Self {
        Self {
            os,
            timing,
            spawn,
            cache: Arc::default(),
        }
    }

    /// Whether the script said `DIRECT` for `name` while it names proxies for other URLs; "no"
    /// while that is not known yet, which starts finding out.
    pub fn direct(&self, name: &str) -> bool {
        let name = name.trim_matches('.').to_ascii_lowercase();
        if name.is_empty() || name.parse::<std::net::IpAddr>().is_ok() {
            return false;
        }
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(&(expires, direct)) = cache.answers.get(&name)
            && Instant::now() < expires
        {
            return direct;
        }
        if cache.pending.len() < MAX_PENDING && cache.pending.insert(name.clone()) {
            let job = Job {
                name,
                os: self.os.clone(),
                timing: self.timing,
                cache: self.cache.clone(),
            };
            drop(cache);
            (self.spawn)(Box::new(move || job.run()));
        }
        false
    }
}

/// One evaluation. Dropping it, run or not, frees its place among the pending names.
struct Job {
    name: String,
    os: Arc<dyn OsProxy>,
    timing: PacTiming,
    cache: Arc<Mutex<Cache>>,
}

impl Job {
    fn run(self) {
        let (direct, ttl) = match self.evaluate() {
            Some(direct) => (direct, self.timing.ttl),
            None => (false, self.timing.failure_ttl),
        };
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.answers.len() >= MAX_NAMES {
            cache.answers.clear();
        }
        cache
            .answers
            .insert(self.name.clone(), (Instant::now() + ttl, direct));
    }

    /// `Some(answer)` when the script was asked and answered, `None` when there is no script or
    /// it failed.
    fn evaluate(&self) -> Option<bool> {
        let config = self.os.config().ok()?;
        if config.pac_url.is_none() && !config.auto_detect {
            return None;
        }
        if !self.names_proxies(config.pac_url.as_deref(), config.auto_detect)? {
            return Some(false);
        }
        let hops = self.ask(config.pac_url.as_deref(), config.auto_detect, &self.name)?;
        Some(matches!(hops.first(), Some(Hop::Direct)))
    }

    fn names_proxies(&self, pac_url: Option<&str>, auto_detect: bool) -> Option<bool> {
        {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((expires, answer)) = cache.names_proxies
                && Instant::now() < expires
            {
                return Some(answer);
            }
        }
        let hops = self.ask(pac_url, auto_detect, REFERENCE_HOST)?;
        let answer = hops.iter().any(|hop| matches!(hop, Hop::Proxy(_)));
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .names_proxies = Some((Instant::now() + self.timing.ttl, answer));
        Some(answer)
    }

    fn ask(&self, pac_url: Option<&str>, auto_detect: bool, host: &str) -> Option<Vec<Hop>> {
        let query = PacQuery {
            pac_url: pac_url.map(str::to_owned),
            auto_detect,
            url: Destination::new(Scheme::Https, host, 443).pac_url(),
            timeout: self.timing.timeout,
        };
        self.os.resolve_pac(&query).ok()
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .remove(&self.name);
    }
}
