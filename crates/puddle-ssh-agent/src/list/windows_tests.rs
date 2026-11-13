// SPDX-License-Identifier: AGPL-3.0-or-later
//! The real pipe: a server that takes the connection and never answers, as a locked agent that asks
//! its user first does.
use super::*;
use tokio::net::windows::named_pipe::ServerOptions;

#[test]
fn a_pipe_that_never_answers_parks_one_request_not_one_per_run() {
    let name = format!(r"\\.\pipe\puddle-ssh-agent-silent-{}", std::process::id());
    let (ready, started) = mpsc::channel();
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server_name = name.clone();
    let server = thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let pipe = ServerOptions::new().create(&server_name).unwrap();
                ready.send(()).unwrap();
                pipe.connect().await.unwrap();
                // Holds the connection and answers nothing until the test ends.
                let _ = stopped.await;
            });
    });
    started.recv().unwrap();
    let endpoint = AgentEndpoint::at(&name);
    let limit = Duration::from_millis(300);

    let first = list_identities(&endpoint, limit);
    assert!(
        matches!(first, Err(AgentError::TimedOut(l)) if l == limit),
        "{first:?}"
    );

    // The first request is parked in its read. A second run must neither connect (the only pipe
    // instance is taken, which would read as unreachable) nor park another thread.
    let second = list_identities(&endpoint, limit);
    assert!(
        matches!(second, Err(AgentError::TimedOut(l)) if l == limit),
        "{second:?}"
    );

    // The server closes its end: the parked read ends and the endpoint is free again.
    stop.send(()).unwrap();
    server.join().unwrap();
    puddle_test_kit::eventually_blocking(
        "the parked request ends",
        Duration::from_secs(10),
        || !parked::is_parked(endpoint.path()),
    );
}
