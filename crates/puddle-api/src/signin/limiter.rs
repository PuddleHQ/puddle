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
//!
//!   An attempt counts from the moment it is let in, not from its answer: an account lets in
//!   only as many attempts at once as it has failures left before the lock (one at a time once
//!   it has been locked), so guesses sent together cannot all be checked before the first
//!   failure is recorded.
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
/// When to come back while an account's allowed attempts are all still being checked, in ms.
pub(crate) const BUSY_MS: u64 = 1_000;
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
    /// Attempts let in whose answer is not known yet.
    in_flight: u32,
}

impl Account {
    /// Attempts this account may have open at once: its failures left before the lock, and one
    /// at a time after a lock.
    fn open_allowed(self) -> u32 {
        FREE_FAILURES.saturating_sub(self.failures).max(1)
    }

    fn idle(self) -> bool {
        self.in_flight == 0 && self.failures == 0
    }
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
    /// back. An attempt that gets through counts against the per-minute limit and stays open
    /// until [`failed`](Self::failed), [`succeeded`](Self::succeeded) or
    /// [`abandoned`](Self::abandoned) closes it.
    pub(crate) fn admit(&mut self, key: &str, now: u64) -> Result<(), TryLater> {
        if let Some(&account) = self.accounts.get(key) {
            if account.locked_until > now {
                return Err(TryLater {
                    after_ms: account.locked_until - now,
                });
            }
            if account.in_flight >= account.open_allowed() {
                return Err(TryLater { after_ms: BUSY_MS });
            }
        }
        self.admit_any(now)?;
        let account = self.account(key, now);
        account.in_flight = account.in_flight.saturating_add(1);
        account.last_try = now;
        Ok(())
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

    /// `key`'s state, made when it has none. When the table is full, the account tried least
    /// recently that has no open attempt is forgotten.
    fn account(&mut self, key: &str, now: u64) -> &mut Account {
        if !self.accounts.contains_key(key) && self.accounts.len() >= MAX_TRACKED {
            let oldest = self
                .accounts
                .iter()
                .filter(|(_, a)| a.in_flight == 0)
                .min_by_key(|(_, a)| a.last_try)
                .map(|(k, _)| k.clone());
            if let Some(oldest) = oldest {
                self.accounts.remove(&oldest);
            }
        }
        self.accounts.entry(key.to_owned()).or_insert(Account {
            failures: 0,
            locked_until: 0,
            last_try: now,
            in_flight: 0,
        })
    }

    /// Closes one open attempt on `key`, and forgets the account when nothing is left to keep.
    fn close(&mut self, key: &str) {
        if let Some(account) = self.accounts.get_mut(key) {
            account.in_flight = account.in_flight.saturating_sub(1);
            if account.idle() {
                self.accounts.remove(key);
            }
        }
    }

    /// A wrong password for `key`: closes its attempt and counts the failure.
    pub(crate) fn failed(&mut self, key: &str, now: u64) {
        let account = self.account(key, now);
        account.in_flight = account.in_flight.saturating_sub(1);
        account.failures = account.failures.saturating_add(1);
        account.last_try = now;
        if account.failures >= FREE_FAILURES {
            account.locked_until = now.saturating_add(lock_ms(account.failures));
        }
    }

    /// The right password for `key`: closes its attempt and forgives the failures before it.
    pub(crate) fn succeeded(&mut self, key: &str) {
        if let Some(account) = self.accounts.get_mut(key) {
            account.failures = 0;
            account.locked_until = 0;
        }
        self.close(key);
    }

    /// An attempt on `key` that ended without an answer (the database failed, the caller went
    /// away): it is closed and counts neither way.
    pub(crate) fn abandoned(&mut self, key: &str) {
        self.close(key);
    }
}

#[cfg(test)]
mod tests;
