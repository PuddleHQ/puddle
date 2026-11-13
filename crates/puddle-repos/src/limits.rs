// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading a Git host's answer: ok, the host's rate limit (and for how long to leave it alone),
//! a rejected token, a refusal. Pure functions over an [`ApiReply`], so every rule has a test on
//! the answers the hosts document.
//!
//! GitHub (REST rate limits, "Handle rate limit errors appropriately"): on `403` or `429`, wait
//! `retry-after` seconds when it is there; else, when `x-ratelimit-remaining` is `0`, until
//! `x-ratelimit-reset`; else (a secondary limit with no header) at least a minute, longer on each
//! repeat. Azure DevOps answers `429` with `Retry-After`.

use serde::Deserialize;

use crate::api::ApiReply;

/// The wait when a host limits us and says nothing about how long: one minute, doubling on each
/// repeat up to an hour.
const MIN_WAIT_MS: u64 = 60_000;
const MAX_WAIT_MS: u64 = 60 * 60_000;
/// How much of a host's own message is kept.
const MESSAGE_CHARS: usize = 200;

/// What an answer means for the request that got it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// A usable answer.
    Ok,
    /// The host does not know the token (expired, revoked, wrong), or answered with its sign-in
    /// page.
    Rejected,
    /// The host limits requests; nothing is asked before `until_ms`.
    Limited {
        /// Epoch milliseconds.
        until_ms: u64,
        /// Whether it is the primary limit (the hour's budget) rather than a secondary one.
        primary: bool,
    },
    /// The host knows the token and refuses this, with its own words.
    Forbidden(Option<String>),
    /// There is nothing at that address (an organisation the sign-in cannot see).
    NotFound,
    /// A status this version has no rule for.
    Other(u16),
}

/// The wait after the `strikes`-th limit in a row (1 for the first).
pub(crate) fn backoff_ms(strikes: u32) -> u64 {
    let doublings = strikes.saturating_sub(1).min(8);
    (MIN_WAIT_MS << doublings).min(MAX_WAIT_MS)
}

fn number(reply: &ApiReply, name: &str) -> Option<u64> {
    reply.header(name)?.trim().parse().ok()
}

/// The host's own `message`, cleaned of control characters and cut short.
pub(crate) fn message_of(reply: &ApiReply) -> Option<String> {
    #[derive(Deserialize)]
    struct Body {
        message: Option<String>,
    }
    let body: Body = serde_json::from_slice(&reply.body).ok()?;
    let text: String = body
        .message?
        .chars()
        .filter(|c| !c.is_control())
        .take(MESSAGE_CHARS)
        .collect();
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// A successful GitHub answer that used up the hour's budget: nothing more is asked until it
/// resets.
pub(crate) fn github_spent(reply: &ApiReply) -> Option<u64> {
    if number(reply, "x-ratelimit-remaining")? != 0 {
        return None;
    }
    Some(number(reply, "x-ratelimit-reset")?.saturating_mul(1000))
}

/// GitHub's answer to one request. `strikes` counts limits in a row, this one included.
pub(crate) fn github(reply: &ApiReply, now_ms: u64, strikes: u32) -> Verdict {
    match reply.status {
        // `304` answers a request that held a current copy; asking for one is the caller's doing.
        200..=299 | 304 => Verdict::Ok,
        401 => Verdict::Rejected,
        404 => Verdict::NotFound,
        403 | 429 => {
            let message = message_of(reply);
            if let Some(seconds) = number(reply, "retry-after") {
                let wait = seconds.saturating_mul(1000).max(backoff_ms(strikes) / 2);
                return limited(now_ms, wait, false);
            }
            if let Some(until) = github_spent(reply) {
                return Verdict::Limited {
                    until_ms: until.max(now_ms.saturating_add(1000)),
                    primary: true,
                };
            }
            let said_limit = message
                .as_deref()
                .is_some_and(|m| m.to_ascii_lowercase().contains("rate limit"));
            if said_limit || reply.status == 429 {
                return limited(now_ms, backoff_ms(strikes), false);
            }
            Verdict::Forbidden(message)
        }
        other => Verdict::Other(other),
    }
}

fn limited(now_ms: u64, wait_ms: u64, primary: bool) -> Verdict {
    Verdict::Limited {
        until_ms: now_ms.saturating_add(wait_ms.min(MAX_WAIT_MS)),
        primary,
    }
}

/// Azure DevOps' answer to one request. It answers a bad or expired token with its sign-in page
/// (`203`, or a redirect, or a `200` of HTML) rather than `401`, so those count as rejected.
pub(crate) fn azure(reply: &ApiReply, now_ms: u64, strikes: u32) -> Verdict {
    if let Some(seconds) = number(reply, "retry-after")
        && reply.status >= 400
    {
        return limited(now_ms, seconds.saturating_mul(1000), false);
    }
    match reply.status {
        200 if is_json(reply) => Verdict::Ok,
        200 | 203 | 301 | 302 | 303 | 307 | 308 | 401 => Verdict::Rejected,
        403 => Verdict::Forbidden(message_of(reply)),
        404 => Verdict::NotFound,
        429 => limited(now_ms, backoff_ms(strikes), false),
        other => Verdict::Other(other),
    }
}

fn is_json(reply: &ApiReply) -> bool {
    reply
        .header("content-type")
        .is_some_and(|t| t.to_ascii_lowercase().contains("json"))
}

#[cfg(test)]
mod tests;
