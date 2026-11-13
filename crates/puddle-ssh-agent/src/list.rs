// SPDX-License-Identifier: GPL-3.0-or-later
//! One request to the host's agent with a time limit: the key list, or a signature.

use std::io::{self, Read, Write};
use std::sync::mpsc::{
    self,
    RecvTimeoutError::{Disconnected, Timeout},
};
use std::thread;
use std::time::Duration;

use crate::endpoint::AgentEndpoint;
use crate::error::AgentError;
use crate::identity::Identity;
use crate::{parked, request, wire};

/// What a request that lost its thread reports.
const THREAD_ENDED: &str = "the request thread ended without an answer";

/// A connection to the agent: a socket or a pipe.
trait Stream: Read + Write {}
impl<S: Read + Write> Stream for S {}

/// One exchange on a fresh connection.
type Exchange<T> = Box<dyn FnOnce(&mut dyn Stream) -> Result<T, AgentError> + Send>;

/// Asks the agent at `endpoint` which keys it holds and waits at most `limit` for the answer,
/// connecting included.
///
/// An agent that asks the user before it answers (1Password, Bitwarden) may wait for them past any
/// limit; the caller then gets [`AgentError::TimedOut`] and the request thread ends when the agent
/// answers or closes. On Windows a read on the pipe cannot be cancelled, so while that thread is
/// parked no further request is sent to the same pipe: each gets [`AgentError::TimedOut`] at once.
///
/// # Errors
///
/// [`AgentError::Unreachable`] when nothing can be opened at the endpoint, [`AgentError::TimedOut`]
/// when the limit passes, otherwise what [`wire::exchange`] reports.
pub fn list_identities(
    endpoint: &AgentEndpoint,
    limit: Duration,
) -> Result<Vec<Identity>, AgentError> {
    within(endpoint, limit, Box::new(|s| wire::exchange(s)))
}

/// Asks the agent at `endpoint` to sign `data` with the key whose public blob is `key` (`flags` as
/// the client sent them) and waits at most `limit`, connecting included. Returns the signature blob
/// as the agent wrote it.
///
/// # Errors
///
/// As [`list_identities`]; [`AgentError::Refused`] when the agent declines (a locked agent, or the
/// user said no at its prompt), [`AgentError::Protocol`] for an answer that is not one signature.
pub fn sign(
    endpoint: &AgentEndpoint,
    key: &[u8],
    data: &[u8],
    flags: u32,
    limit: Duration,
) -> Result<Vec<u8>, AgentError> {
    let ask = request::sign_request(key, data, flags);
    within(
        endpoint,
        limit,
        Box::new(move |s| request::parse_signature(&wire::ask(s, &ask)?)),
    )
}

fn within<T: Send + 'static>(
    endpoint: &AgentEndpoint,
    limit: Duration,
    exchange: Exchange<T>,
) -> Result<T, AgentError> {
    // A pipe read cannot be cancelled (see `parked`); a Unix socket read ends at its own limit.
    within_gated(endpoint, limit, exchange, cfg!(windows))
}

/// [`within`], where `one_parked_at_most` refuses to start a request at an endpoint whose earlier
/// request is still parked in its read after its caller gave up.
fn within_gated<T: Send + 'static>(
    endpoint: &AgentEndpoint,
    limit: Duration,
    exchange: Exchange<T>,
    one_parked_at_most: bool,
) -> Result<T, AgentError> {
    if one_parked_at_most && parked::is_parked(endpoint.path()) {
        return Err(AgentError::TimedOut(limit));
    }
    let (sender, receiver) = mpsc::channel();
    let endpoint = endpoint.clone();
    let request = parked::Request::new(endpoint.path());
    let thread_request = request.clone();
    let worker = thread::Builder::new()
        .name("ssh-agent-request".into())
        .spawn(move || {
            // The receiver is gone when the limit passed first; nobody wants the answer then.
            let _late = sender.send(ask(&endpoint, limit, exchange));
            thread_request.finished();
        })
        .map_err(AgentError::Io)?;
    match receiver.recv_timeout(limit) {
        Ok(result) => {
            // The thread has sent its answer and is ending.
            let _joined = worker.join();
            result
        }
        Err(Timeout) => {
            request.abandoned();
            Err(AgentError::TimedOut(limit))
        }
        // The thread ended without an answer: it panicked.
        Err(Disconnected) => Err(AgentError::Io(io::Error::other(THREAD_ENDED))),
    }
}

/// One request on the host's kind of endpoint. The two bodies have their own names so that a
/// mutation exclusion can name the Windows one (a Linux run never compiles it).
fn ask<T>(
    endpoint: &AgentEndpoint,
    limit: Duration,
    exchange: Exchange<T>,
) -> Result<T, AgentError> {
    #[cfg(unix)]
    {
        ask_socket(endpoint, limit, exchange)
    }
    #[cfg(windows)]
    {
        ask_pipe(endpoint, limit, exchange)
    }
}

#[cfg(unix)]
fn ask_socket<T>(
    endpoint: &AgentEndpoint,
    limit: Duration,
    exchange: Exchange<T>,
) -> Result<T, AgentError> {
    let mut stream = std::os::unix::net::UnixStream::connect(endpoint.path())
        .map_err(AgentError::Unreachable)?;
    // The caller stops waiting at `limit`; this makes the thread stop reading then too.
    stream
        .set_read_timeout(Some(limit))
        .and_then(|()| stream.set_write_timeout(Some(limit)))
        .map_err(AgentError::Io)?;
    exchange(&mut stream).map_err(|error| as_timeout(error, limit))
}

/// A socket timeout set in [`ask_socket`] surfaces as `WouldBlock` (Linux, macOS) or `TimedOut`. When the
/// request thread hits it just before the caller's own wait ends, the thread's error is the one
/// the caller reads, so it must say what the caller would have said.
#[cfg(unix)]
fn as_timeout(error: AgentError, limit: Duration) -> AgentError {
    match error {
        AgentError::Io(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
            ) =>
        {
            AgentError::TimedOut(limit)
        }
        other => other,
    }
}

#[cfg(windows)]
fn ask_pipe<T>(
    endpoint: &AgentEndpoint,
    // Nothing here can enforce it; `within_gated` bounds the cost of a read that never ends.
    _limit: Duration,
    exchange: Exchange<T>,
) -> Result<T, AgentError> {
    // A named pipe opens like a file. A pipe whose instances are all busy fails here at once
    // (`ERROR_PIPE_BUSY`), which reads as unreachable.
    let mut pipe = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(endpoint.path())
        .map_err(AgentError::Unreachable)?;
    exchange(&mut pipe)
}

#[cfg(not(any(unix, windows)))]
compile_error!("puddle-ssh-agent supports unix and windows hosts");

#[cfg(all(test, unix))]
mod tests;

#[cfg(all(test, windows))]
mod windows_tests;
