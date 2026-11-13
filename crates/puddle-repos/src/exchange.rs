// SPDX-License-Identifier: AGPL-3.0-or-later
//! One request to a Git host and what its answer means, in words for the user.

use puddle_store::Clock;

use crate::api::{Api, ApiReply, ApiRequest, TransportError};
use crate::limits::Verdict;
use crate::model::{Problem, ProblemKind};

/// What the user does about a token the host does not accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Remedy {
    /// A signed-in account (`gh`, Git Credential Manager): sign in again.
    SignIn,
    /// A pasted token: paste a new one.
    NewToken,
}

impl Remedy {
    fn text(self) -> &'static str {
        match self {
            Self::SignIn => "sign in again",
            Self::NewToken => "paste a new token",
        }
    }
}

/// The host a request goes to and how to read its answers.
pub(crate) struct Exchange<'a> {
    pub(crate) api: &'a dyn Api,
    pub(crate) clock: &'a dyn Clock,
    /// Limits in a row so far, this request's not yet counted.
    pub(crate) strikes: u32,
    /// The host's name for the user: `GitHub`, `Azure DevOps`.
    pub(crate) who: &'static str,
    pub(crate) remedy: Remedy,
    /// Reads a status and headers into a verdict.
    pub(crate) judge: fn(&ApiReply, u64, u32) -> Verdict,
    /// What a `404` is about, for the message.
    pub(crate) missing: String,
}

impl Exchange<'_> {
    /// Sends `request`; a usable answer comes back, anything else as the problem to show.
    pub(crate) async fn get(&self, request: ApiRequest) -> Result<ApiReply, Problem> {
        let host = request.host.clone();
        let reply = self
            .api
            .get(request)
            .await
            .map_err(|err| transport_problem(&host, &err))?;
        match (self.judge)(&reply, self.clock.now_ms(), self.strikes.saturating_add(1)) {
            Verdict::Ok => Ok(reply),
            other => Err(self.problem(other)),
        }
    }

    fn problem(&self, verdict: Verdict) -> Problem {
        let who = self.who;
        match verdict {
            // Not reached: `get` returns an `Ok` answer as it is.
            Verdict::Ok => Problem::bad_answer("an answer that needs no explanation"),
            Verdict::Rejected => Problem {
                needs_sign_in: self.remedy == Remedy::SignIn,
                ..Problem::new(
                    ProblemKind::TokenRejected,
                    format!(
                        "{who} did not accept the token; it may have expired or been revoked: {}",
                        self.remedy.text()
                    ),
                )
            },
            Verdict::Limited { until_ms, primary } => Problem {
                retry_at: Some(until_ms),
                ..Problem::new(
                    ProblemKind::RateLimited,
                    if primary {
                        format!("{who}'s rate limit is used up; Puddle asks again once it resets")
                    } else {
                        format!(
                            "{who} is limiting how fast Puddle may ask; Puddle waits before it asks again"
                        )
                    },
                )
            },
            Verdict::Forbidden(Some(said)) => Problem::new(
                ProblemKind::Forbidden,
                format!("{who} refused the request: {said}"),
            ),
            Verdict::Forbidden(None) => Problem::new(
                ProblemKind::Forbidden,
                format!("{who} refused the request (403) and gave no reason"),
            ),
            Verdict::NotFound => Problem::new(
                ProblemKind::NotFound,
                format!("{who} has no {} that this token can see", self.missing),
            ),
            Verdict::Other(status) => Problem::bad_answer(format!(
                "{who} answered {status}, which Puddle has no rule for"
            )),
        }
    }
}

fn transport_problem(host: &str, err: &TransportError) -> Problem {
    match err {
        TransportError::Tls(why) => Problem::new(
            ProblemKind::Unreachable,
            format!(
                "the connection to {host} is not trusted: {why}; if the company network re-signs \
                 HTTPS, its root certificate must be trusted by this computer"
            ),
        ),
        TransportError::TooLarge => Problem::bad_answer(err),
        TransportError::BadToken => Problem::new(ProblemKind::SourceUnavailable, err.to_string()),
        other => Problem::unreachable(format!("{host}: {other}")),
    }
}

#[cfg(test)]
mod tests;
