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

/// One request on the host's kind of endpoint. The two bodies have their own names so that a
/// mutation exclusion can name the Windows one (a Linux run never compiles it).
fn ask(endpoint: &AgentEndpoint, limit: Duration) -> Result<Vec<Identity>, AgentError> {
    #[cfg(unix)]
    {
        ask_socket(endpoint, limit)
    }
    #[cfg(windows)]
    {
        ask_pipe(endpoint, limit)
    }
}

#[cfg(unix)]
fn ask_socket(endpoint: &AgentEndpoint, limit: Duration) -> Result<Vec<Identity>, AgentError> {
    let mut stream = std::os::unix::net::UnixStream::connect(endpoint.path())
        .map_err(AgentError::Unreachable)?;
    // The caller stops waiting at `limit`; this makes the thread stop reading then too.
    stream
        .set_read_timeout(Some(limit))
        .and_then(|()| stream.set_write_timeout(Some(limit)))
        .map_err(AgentError::Io)?;
    wire::exchange(&mut stream).map_err(|error| as_timeout(error, limit))
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
fn ask_pipe(endpoint: &AgentEndpoint, _limit: Duration) -> Result<Vec<Identity>, AgentError> {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// The request thread's own read timeout, reached before the caller's wait ends (a loaded host
    /// schedules the caller late), must be the same answer the caller gives.
    #[test]
    fn a_read_timeout_on_the_request_thread_is_a_timeout_not_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("silent.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let limit = Duration::from_millis(50);
        let result = ask(&AgentEndpoint::at(&path), limit);
        assert!(
            matches!(result, Err(AgentError::TimedOut(l)) if l == limit),
            "{result:?}"
        );
        drop(listener);
    }
}
