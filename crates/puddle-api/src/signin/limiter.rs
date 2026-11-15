// SPDX-License-Identifier: AGPL-3.0-or-later
//! The limits on password attempts. Puddle never trusts `X-Forwarded-*`, so it cannot tell two
//! clients behind one proxy apart: there is no per-address limit. The limits are on what the
//! attacker has to guess, and on the work the server does:
//!
//! - **Per account:** after [`FREE_FAILURES`] wrong passwords in a row the account is locked, for
//!   [`BASE_LOCK_MS`] first and twice as long for each further failure, up to [`MAX_LOCK_MS`].
//!   While it is locked even the right password is refused, so a lock gives a guesser nothing to
//!   learn from. A name nobody has is tracked the same way, so the answers do not tell names
//!   apart. The price is that someone who knows the owner's name can keep the owner's
//!   *password sign-in* locked; devices already signed in are not affected, and the server's
//!   command line can still reset the password.
//! - **Everywhere:** at most [`PER_MINUTE`] password checks (each costs 19 MiB and tens of
//!   milliseconds) in any minute, whoever asks.
//!
//! State is in memory: a restart clears it, and an attacker cannot restart the server.

use std::collections::{HashMap, VecDeque};

/// Wrong passwords in a row an account takes before it locks.
pub(crate) const FREE_FAILURES: u32 = 5;
/// The first lock, in ms.
pub(crate) const BASE_LOCK_MS: u64 = 30_000;
/// The longest lock, in ms.
pub(crate) const MAX_LOCK_MS: u64 = 15 * 60_000;
/// Password checks allowed in any minute.
pub(crate) const PER_MINUTE: usize = 30;
/// Accounts tracked at once; the least recently tried are forgotten first.
const MAX_TRACKED: usize = 4096;
const MINUTE_MS: u64 = 60_000;

/// Why an attempt was not made, and when to try again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TryLater {
    /// Milliseconds until the limit lets an attempt through.
    pub(crate) after_ms: u64,
}

impl TryLater {
    /// Whole seconds, at least one, for `Retry-After`.
    pub(crate) fn seconds(self) -> u64 {
        self.after_ms.div_ceil(1000).max(1)
    }
}

#[derive(Debug, Clone, Copy)]
struct Account {
    failures: u32,
    locked_until: u64,
    last_try: u64,
}

/// The limits' state.
#[derive(Debug, Default)]
pub(crate) struct Limiter {
    accounts: HashMap<String, Account>,
    recent: VecDeque<u64>,
}

/// How long the lock after `failures` wrong passwords in a row lasts.
fn lock_ms(failures: u32) -> u64 {
    let doublings = failures.saturating_sub(FREE_FAILURES).min(16);
    BASE_LOCK_MS
        .saturating_mul(1u64 << doublings)
        .min(MAX_LOCK_MS)
}

impl Limiter {
    /// Lets one attempt on `key` (a normalised account name) through, or says when to come
    /// back. An attempt that gets through counts against the per-minute limit.
    pub(crate) fn admit(&mut self, key: &str, now: u64) -> Result<(), TryLater> {
        if let Some(account) = self.accounts.get(key)
            && account.locked_until > now
        {
            return Err(TryLater {
                after_ms: account.locked_until - now,
            });
        }
        self.admit_any(now)
    }

    /// The per-minute limit alone (an attempt that has no account to blame, like an invite).
    pub(crate) fn admit_any(&mut self, now: u64) -> Result<(), TryLater> {
        while self
            .recent
            .front()
            .is_some_and(|&at| at.saturating_add(MINUTE_MS) <= now)
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= PER_MINUTE {
            let oldest = self.recent.front().copied().unwrap_or(now);
            return Err(TryLater {
                after_ms: oldest.saturating_add(MINUTE_MS).saturating_sub(now),
            });
        }
        self.recent.push_back(now);
        Ok(())
    }

    /// A wrong password for `key`.
    pub(crate) fn failed(&mut self, key: &str, now: u64) {
        if !self.accounts.contains_key(key) && self.accounts.len() >= MAX_TRACKED {
            let oldest = self
                .accounts
                .iter()
                .min_by_key(|(_, a)| a.last_try)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.accounts.remove(&oldest);
            }
        }
        let account = self.accounts.entry(key.to_owned()).or_insert(Account {
            failures: 0,
            locked_until: 0,
            last_try: now,
        });
        account.failures = account.failures.saturating_add(1);
        account.last_try = now;
        if account.failures >= FREE_FAILURES {
            account.locked_until = now.saturating_add(lock_ms(account.failures));
        }
    }

    /// The right password for `key`: its failures are forgiven.
    pub(crate) fn succeeded(&mut self, key: &str) {
        self.accounts.remove(key);
    }
}

#[cfg(test)]
mod tests;
