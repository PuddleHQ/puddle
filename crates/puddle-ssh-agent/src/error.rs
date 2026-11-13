// SPDX-License-Identifier: AGPL-3.0-or-later
//! Why a key-list request failed.

use std::io;
use std::time::Duration;

/// Why the agent's key list could not be had.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Nothing listens at the endpoint (no such pipe or socket), or this user may not open it.
    #[error("cannot connect: {0}")]
    Unreachable(#[source] io::Error),
    /// The agent did not answer within the limit; an agent that asks the user first may be waiting
    /// for them.
    #[error("no answer within {0:?}")]
    TimedOut(Duration),
    /// The agent answered "failure": an agent that is locked does this to every request.
    #[error("the agent refused the request (locked?)")]
    Refused,
    /// The connection broke while talking to the agent.
    #[error("the connection to the agent broke: {0}")]
    Io(#[source] io::Error),
    /// The agent's answer is not valid agent protocol.
    #[error("the agent's answer is not valid: {0}")]
    Protocol(&'static str),
}
