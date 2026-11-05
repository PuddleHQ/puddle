// SPDX-License-Identifier: GPL-3.0-or-later
//! One key-list request with a time limit.

use std::io;
use std::sync::mpsc::{
    self,
    RecvTimeoutError::{Disconnected, Timeout},
};
use std::thread;
use std::time::Duration;

use crate::endpoint::AgentEndpoint;
use crate::error::AgentError;
use crate::identity::Identity;
use crate::wire;

/// What a request that lost its thread reports.
const THREAD_ENDED: &str = "the request thread ended without an answer";

/// Asks the agent at `endpoint` which keys it holds and waits at most `limit` for the answer,
/// connecting included.
///
/// An agent that asks the user before it answers (1Password, Bitwarden) may wait for them past any
/// limit; the caller then gets [`AgentError::TimedOut`] and the request thread ends when the agent
/// answers or closes.
///
/// # Errors
///
/// [`AgentError::Unreachable`] when nothing can be opened at the endpoint, [`AgentError::TimedOut`]
/// when the limit passes, otherwise what [`wire::exchange`] reports.
pub fn list_identities(
    endpoint: &AgentEndpoint,
    limit: Duration,
) -> Result<Vec<Identity>, AgentError> {
    let (sender, receiver) = mpsc::channel();
    let endpoint = endpoint.clone();
    let worker = thread::Builder::new()
        .name("ssh-agent-list".into())
        .spawn(move || {
            // The receiver is gone when the limit passed first; nobody wants the answer then.
            let _late = sender.send(ask(&endpoint, limit));
        })
        .map_err(AgentError::Io)?;
    match receiver.recv_timeout(limit) {
        Ok(result) => {
            // The thread has sent its answer and is ending.
            let _joined = worker.join();
            result
        }
        Err(Timeout) => Err(AgentError::TimedOut(limit)),
        // The thread ended without an answer: it panicked.
        Err(Disconnected) => Err(AgentError::Io(io::Error::other(THREAD_ENDED))),
    }
}

#[cfg(unix)]
fn ask(endpoint: &AgentEndpoint, limit: Duration) -> Result<Vec<Identity>, AgentError> {
    let mut stream = std::os::unix::net::UnixStream::connect(endpoint.path())
        .map_err(AgentError::Unreachable)?;
    // The caller stops waiting at `limit`; this makes the thread stop reading then too.
    stream
        .set_read_timeout(Some(limit))
        .and_then(|()| stream.set_write_timeout(Some(limit)))
        .map_err(AgentError::Io)?;
    wire::exchange(&mut stream)
}

#[cfg(windows)]
fn ask(endpoint: &AgentEndpoint, _limit: Duration) -> Result<Vec<Identity>, AgentError> {
    // A named pipe opens like a file. A pipe whose instances are all busy fails here at once
    // (`ERROR_PIPE_BUSY`), which reads as unreachable.
    let mut pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(endpoint.path())
        .map_err(AgentError::Unreachable)?;
    wire::exchange(&mut pipe)
}

#[cfg(not(any(unix, windows)))]
compile_error!("puddle-ssh-agent supports unix and windows hosts");
