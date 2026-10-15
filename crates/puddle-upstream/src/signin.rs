// SPDX-License-Identifier: GPL-3.0-or-later
//! What the last sign-in to each upstream proxy came to, for the network-health report. Holds
//! scheme words and short messages only: never a token, a header value or a password.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::SystemTime;

use tokio::sync::broadcast;

use crate::hop::ProxyAddr;
use crate::redact::redact_text;

/// How a sign-in to a proxy ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignInOutcome {
    /// The proxy asked, and Puddle answered.
    SignedIn,
    /// The proxy let the request through without asking.
    NotRequired,
    /// The proxy refused the credentials or the token, or asked too often.
    Failed,
    /// The proxy asks for something Puddle has no way to give (a scheme it does not speak, or no
    /// configured password).
    Unsupported,
}

/// The latest sign-in attempt to one proxy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignIn {
    /// The proxy.
    pub proxy: ProxyAddr,
    /// The scheme word that was sent (`Negotiate`, `NTLM`, `Basic`); `None` when nothing was.
    pub scheme: Option<String>,
    /// How it ended.
    pub outcome: SignInOutcome,
    /// Why, in words, already cleaned of anything secret.
    pub detail: Option<String>,
    /// When.
    pub at: SystemTime,
}

/// How many failure notices wait for a slow receiver before the oldest are dropped.
const NOTICES: usize = 16;

#[derive(Debug)]
pub(crate) struct SignInLog {
    latest: Mutex<HashMap<ProxyAddr, SignIn>>,
    failures: broadcast::Sender<SignIn>,
}

impl Default for SignInLog {
    fn default() -> Self {
        Self {
            latest: Mutex::default(),
            failures: broadcast::channel(NOTICES).0,
        }
    }
}

impl SignInLog {
    /// A receiver of the sign-ins that went from not failing to failing: one per proxy per
    /// episode, so a client that retries a wrong password gets one notice, and another after a
    /// sign-in worked in between.
    pub(crate) fn failures(&self) -> broadcast::Receiver<SignIn> {
        self.failures.subscribe()
    }

    pub(crate) fn record(
        &self,
        proxy: &ProxyAddr,
        scheme: Option<String>,
        outcome: SignInOutcome,
        detail: Option<String>,
    ) {
        let entry = SignIn {
            proxy: proxy.clone(),
            scheme,
            outcome,
            detail: detail.map(|text| redact_text(&text)),
            at: SystemTime::now(),
        };
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        let was_failing = latest
            .get(proxy)
            .is_some_and(|before| before.outcome == SignInOutcome::Failed);
        if outcome == SignInOutcome::Failed && !was_failing {
            // No receiver is fine: nobody is told, and the report still lists the failure.
            let _ = self.failures.send(entry.clone());
        }
        latest.insert(proxy.clone(), entry);
    }

    /// Newest first.
    pub(crate) fn all(&self) -> Vec<SignIn> {
        let mut all: Vec<SignIn> = self
            .latest
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect();
        all.sort_by(|a, b| b.at.cmp(&a.at).then_with(|| a.proxy.cmp(&b.proxy)));
        all
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;

    fn proxy() -> ProxyAddr {
        ProxyAddr::new("proxy.corp", 8080)
    }

    #[test]
    fn a_failure_is_announced_once_per_episode_and_never_with_a_secret() {
        let log = SignInLog::default();
        let mut failures = log.failures();
        let record = |outcome, detail: Option<&str>| {
            log.record(
                &proxy(),
                Some("Basic".into()),
                outcome,
                detail.map(str::to_owned),
            );
        };
        record(SignInOutcome::SignedIn, None);
        assert_eq!(failures.try_recv().unwrap_err(), TryRecvError::Empty);

        record(SignInOutcome::Failed, Some("rejected Basic abc123=="));
        let notice = failures.try_recv().unwrap();
        assert_eq!(notice.proxy, proxy());
        assert_eq!(notice.scheme.as_deref(), Some("Basic"));
        assert_eq!(notice.outcome, SignInOutcome::Failed);
        assert!(
            !format!("{notice:?}").contains("abc123"),
            "the notice is cleaned like the report: {notice:?}"
        );

        // A client retrying the same wrong password is the same episode.
        record(SignInOutcome::Failed, None);
        assert_eq!(failures.try_recv().unwrap_err(), TryRecvError::Empty);
        // Unsupported is not a failed attempt: nothing to correct by retrying.
        record(SignInOutcome::Unsupported, None);
        assert_eq!(failures.try_recv().unwrap_err(), TryRecvError::Empty);
        // After it worked, the next failure is news again; another proxy has its own episode.
        record(SignInOutcome::SignedIn, None);
        record(SignInOutcome::Failed, None);
        assert!(failures.try_recv().is_ok());
        log.record(
            &ProxyAddr::new("other.corp", 3128),
            None,
            SignInOutcome::Failed,
            None,
        );
        assert_eq!(failures.try_recv().unwrap().scheme, None);
    }

    #[test]
    fn with_nobody_listening_a_failure_is_still_in_the_report() {
        let log = SignInLog::default();
        log.record(&proxy(), None, SignInOutcome::Failed, Some("no".into()));
        assert_eq!(log.all().len(), 1);
    }
}
