// SPDX-License-Identifier: AGPL-3.0-or-later
//! Scripted answers in place of a Git host, for tests of this crate and of the crates that use it.
//! Never in product code.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};

use futures_util::future::BoxFuture;

use crate::api::{Api, ApiReply, ApiRequest, TransportError};

/// A request the fake saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Seen {
    /// The API host asked.
    pub host: String,
    /// The path and query asked.
    pub path: String,
    /// The token that was sent (a test's made-up value), so a test can prove which credential
    /// went where.
    pub token: String,
    /// Whether it went as `Bearer` (otherwise as the password of a Basic header).
    pub bearer: bool,
    /// The `If-None-Match` it carried.
    pub if_none_match: Option<String>,
}

#[derive(Default)]
struct State {
    script: HashMap<(String, String), VecDeque<Result<ApiReply, TransportError>>>,
    seen: Vec<Seen>,
}

/// Answers each (host, path) from a queue, the last answer repeating; anything not scripted is
/// unreachable.
#[derive(Default)]
pub struct FakeApi {
    state: Mutex<State>,
}

impl std::fmt::Debug for FakeApi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeApi").finish_non_exhaustive()
    }
}

impl FakeApi {
    /// A fake with nothing scripted.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Queues an answer for `host` and `path` (the path with its query, as asked).
    pub fn reply(&self, host: &str, path: &str, reply: ApiReply) {
        self.queue(host, path, Ok(reply));
    }

    /// Forgets what was queued for `host` and `path`, so the next answer queued is the only one.
    pub fn clear(&self, host: &str, path: &str) {
        self.state()
            .script
            .remove(&(host.to_owned(), path.to_owned()));
    }

    /// Queues a transport failure.
    pub fn fail(&self, host: &str, path: &str, err: TransportError) {
        self.queue(host, path, Err(err));
    }

    fn queue(&self, host: &str, path: &str, answer: Result<ApiReply, TransportError>) {
        self.state()
            .script
            .entry((host.to_owned(), path.to_owned()))
            .or_default()
            .push_back(answer);
    }

    /// Every request seen so far, in order.
    #[must_use]
    pub fn seen(&self) -> Vec<Seen> {
        self.state().seen.clone()
    }

    /// How many requests were seen.
    #[must_use]
    pub fn count(&self) -> usize {
        self.state().seen.len()
    }
}

impl Api for FakeApi {
    fn get(&self, request: ApiRequest) -> BoxFuture<'_, Result<ApiReply, TransportError>> {
        Box::pin(async move {
            let mut state = self.state();
            state.seen.push(Seen {
                host: request.host.clone(),
                path: request.path.clone(),
                token: request.authorization.token().to_owned(),
                bearer: request.authorization.is_bearer(),
                if_none_match: request.if_none_match.clone(),
            });
            let key = (request.host.clone(), request.path.clone());
            let Some(queue) = state.script.get_mut(&key) else {
                return Err(TransportError::Unreachable(format!(
                    "nothing scripted for {} {}",
                    request.host, request.path
                )));
            };
            let next = if queue.len() > 1 {
                queue.pop_front()
            } else {
                queue.front().cloned()
            };
            next.unwrap_or(Err(TransportError::Protocol("an empty queue".to_owned())))
        })
    }
}

#[cfg(test)]
mod tests;
