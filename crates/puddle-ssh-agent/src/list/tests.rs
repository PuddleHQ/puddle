// SPDX-License-Identifier: AGPL-3.0-or-later
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
    let result = ask(
        &AgentEndpoint::at(&path),
        limit,
        Box::new(|s| wire::exchange(s)),
    );
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

/// On Windows a pipe read cannot be cancelled, so a request whose caller gave up stays parked; a
/// second request to that endpoint must not park another thread and handle behind it.
#[test]
fn no_second_request_goes_to_an_endpoint_that_has_a_parked_one() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("locked.sock");
    let _listener = UnixListener::bind(&path).unwrap();
    let endpoint = AgentEndpoint::at(&path);
    let limit = Duration::from_millis(100);
    let sent = Arc::new(AtomicUsize::new(0));
    let (open, gate) = mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(gate));
    let request =
        |sent: &Arc<AtomicUsize>, gate: &Arc<Mutex<mpsc::Receiver<()>>>| -> Exchange<()> {
            let (sent, gate) = (sent.clone(), gate.clone());
            Box::new(move |_stream| {
                sent.fetch_add(1, Ordering::SeqCst);
                // Parked like a pipe read: only the agent answering (the gate) ends it.
                let _ = gate.lock().unwrap().recv();
                Ok(())
            })
        };

    let first = within_gated(&endpoint, limit, request(&sent, &gate), true);
    assert!(
        matches!(first, Err(AgentError::TimedOut(l)) if l == limit),
        "{first:?}"
    );
    assert!(parked::is_parked(endpoint.path()));

    let second = within_gated(&endpoint, limit, request(&sent, &gate), true);
    assert!(
        matches!(second, Err(AgentError::TimedOut(l)) if l == limit),
        "{second:?}"
    );
    assert_eq!(
        sent.load(Ordering::SeqCst),
        1,
        "the second request was sent"
    );

    // Without the gate (a Unix socket) every request goes out.
    let ungated = within_gated(&endpoint, limit, request(&sent, &gate), false);
    assert!(
        matches!(ungated, Err(AgentError::TimedOut(_))),
        "{ungated:?}"
    );
    assert_eq!(sent.load(Ordering::SeqCst), 2);

    // The agent answers: both parked threads end and the endpoint takes requests again.
    open.send(()).unwrap();
    open.send(()).unwrap();
    puddle_test_kit::eventually_blocking(
        "the endpoint stops being parked",
        Duration::from_secs(10),
        || !parked::is_parked(endpoint.path()),
    );
    open.send(()).unwrap();
    let third = within_gated(
        &endpoint,
        Duration::from_secs(10),
        request(&sent, &gate),
        true,
    );
    assert!(third.is_ok(), "{third:?}");
    assert_eq!(sent.load(Ordering::SeqCst), 3);
}
