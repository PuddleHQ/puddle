// SPDX-License-Identifier: AGPL-3.0-or-later
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
mod tests;
