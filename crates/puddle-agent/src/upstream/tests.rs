// SPDX-License-Identifier: AGPL-3.0-or-later
#[cfg(unix)]
use std::time::Duration;

use super::*;

fn upstream(target: Target) -> Upstream {
    Upstream::new(&Config {
        target,
        mux: 2,
        ..Config::default()
    })
}

#[tokio::test]
async fn an_unreachable_host_is_an_error_on_every_slot() {
    let up = upstream(Target::Unix("/nonexistent/puddle-route.sock".into()));
    assert!(format!("{up:?}").contains("sessions: 2"));
    for _ in 0..3 {
        assert!(up.open().await.is_err());
    }
}

/// A host that serves yamux on a Unix socket and keeps every session so a test can end it.
#[cfg(unix)]
struct Host {
    sessions: Arc<std::sync::Mutex<Vec<tokio::task::AbortHandle>>>,
    accepted: Arc<AtomicUsize>,
    path: PathBuf,
    _dir: tempfile::TempDir,
}

#[cfg(unix)]
impl Host {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("route.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let sessions = Arc::new(std::sync::Mutex::new(Vec::new()));
        let kept = Arc::clone(&sessions);
        let accepted = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                let mut session =
                    Session::new_server(conn, puddle_agent_proto::yamux::server_config());
                let serving = tokio::spawn(async move {
                    // Accepted streams are dropped; the sessions only need to exist.
                    let mut held = Vec::new();
                    while let Some(Ok(stream)) = session.next().await {
                        held.push(stream);
                    }
                });
                kept.lock().unwrap().push(serving.abort_handle());
                counted.fetch_add(1, Ordering::SeqCst);
            }
        });
        Self {
            sessions,
            accepted,
            path,
            _dir: dir,
        }
    }

    /// How many connections the host has accepted.
    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }

    /// Cuts every session the host serves, as a host that restarted would.
    fn end_sessions(&self) {
        for session in self.sessions.lock().unwrap().drain(..) {
            session.abort();
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_session_the_host_ended_is_redialled_by_the_next_open() {
    let host = Host::start();
    let up = Upstream::new(&Config {
        target: Target::Unix(host.path.clone()),
        mux: 1,
        ..Config::default()
    });
    up.open().await.unwrap();
    assert_eq!(host.accepted(), 1);
    host.end_sessions();
    puddle_test_kit::eventually_async(
        "a stream on a new session",
        Duration::from_secs(5),
        || async { up.open().await.is_ok() },
    )
    .await;
    assert_eq!(host.accepted(), 1, "the new session was dialled once");
}

#[cfg(unix)]
#[tokio::test]
async fn a_stream_open_when_its_session_ends_fails_instead_of_hanging() {
    use tokio::io::AsyncReadExt;

    let host = Host::start();
    let up = Upstream::new(&Config {
        target: Target::Unix(host.path.clone()),
        mux: 1,
        ..Config::default()
    });
    let mut stream = up.open().await.unwrap();
    host.end_sessions();
    let mut buf = [0u8; 1];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("the stream hung after its session ended");
    assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_vsock_target_nobody_listens_on_is_an_error() {
    // CID 1 is local loopback; nothing listens there.
    let up = upstream(Target::Vsock {
        cid: 1,
        port: 0x7fff_fff1,
    });
    assert!(up.open().await.is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn a_named_pipe_is_dialled_and_a_missing_one_is_an_error() {
    use tokio::net::windows::named_pipe::ServerOptions;

    let name = format!(r"\\.\pipe\puddle-agent-dial-{}", std::process::id());
    let _server = ServerOptions::new().create(&name).unwrap();
    dial_unix(std::path::Path::new(&name)).await.unwrap();
    let missing = format!("{name}-missing");
    assert!(dial_unix(std::path::Path::new(&missing)).await.is_err());
}
