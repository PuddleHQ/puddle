// SPDX-License-Identifier: GPL-3.0-or-later
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

#[test]
fn only_would_block_and_timed_out_become_a_timeout() {
    let limit = Duration::from_secs(3);
    for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::TimedOut] {
        let got = as_timeout(AgentError::Io(io::Error::from(kind)), limit);
        assert!(
            matches!(got, AgentError::TimedOut(l) if l == limit),
            "{kind:?}: {got:?}"
        );
    }
    for kind in [
        io::ErrorKind::BrokenPipe,
        io::ErrorKind::UnexpectedEof,
        io::ErrorKind::Interrupted,
    ] {
        let got = as_timeout(AgentError::Io(io::Error::from(kind)), limit);
        assert!(
            matches!(&got, AgentError::Io(e) if e.kind() == kind),
            "{kind:?}: {got:?}"
        );
    }
}
