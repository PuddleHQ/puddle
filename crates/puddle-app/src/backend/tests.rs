// SPDX-License-Identifier: AGPL-3.0-or-later
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

use puddle_host::Step;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::*;

/// One HTTP/1.1 GET over loopback; returns the whole reply as text.
pub(crate) async fn get(backend: &Backend, path: &str, token: Option<&str>) -> String {
    let addr = (
        backend.url().host_str().unwrap().to_owned(),
        backend.url().port().unwrap(),
    );
    let mut stream = TcpStream::connect(addr.clone()).await.unwrap();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let host = format!("{}:{}", backend.url().host_str().unwrap(), addr.1);
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{auth}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.unwrap();
    String::from_utf8_lossy(&reply).into_owned()
}

pub(crate) fn fixture_options() -> Options {
    Options {
        fixture: true,
        ..Options::default()
    }
}

pub(crate) async fn start_fixture(port: u16) -> Backend {
    let options = Options {
        port,
        ..fixture_options()
    };
    Source::Fixture.start(&options).await.unwrap()
}

#[tokio::test]
async fn the_fixture_serves_the_api_on_loopback_with_its_token() {
    let backend = start_fixture(0).await;
    assert_eq!(backend.url().host_str(), Some("127.0.0.1"));
    assert_ne!(backend.url().port(), Some(0));
    assert_eq!(backend.token().len(), 64);

    let with = get(&backend, "/api/pending", Some(backend.token())).await;
    assert!(with.starts_with("HTTP/1.1 200"), "{with}");
    assert!(with.contains("registry.example.org"), "{with}");

    let without = get(&backend, "/api/pending", None).await;
    assert!(without.starts_with("HTTP/1.1 401"), "{without}");
    backend.shutdown().await;
}

/// The Windows smoke test goes through the first-run flow in the window, so the stand-in
/// backend has to start as a fresh install; seeding it as set up would hide the flow.
#[tokio::test]
async fn the_fixture_is_a_fresh_install_whose_first_run_flow_is_open() {
    let backend = start_fixture(0).await;
    let reply = get(&backend, "/api/first-run", Some(backend.token())).await;
    assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
    assert!(reply.contains("\"completed\":false"), "{reply}");
    backend.shutdown().await;
}

#[tokio::test]
async fn the_fixture_runs_no_workspaces_so_it_has_no_extension_hosts_to_end() {
    let backend = start_fixture(0).await;
    let demo = WorkspaceId::new("demo").unwrap();
    let reason = (backend.extension_hosts())(demo).await.unwrap_err();
    assert_eq!(reason, "this backend runs no workspaces");
    backend.shutdown().await;
}

#[tokio::test]
async fn the_fixture_runs_no_workspaces_so_it_has_no_terminals_open() {
    let backend = start_fixture(0).await;
    assert_eq!(backend.open_terminals(), 0);
    backend.shutdown().await;
}

#[tokio::test]
async fn the_fixture_stops_only_its_api_and_a_second_shutdown_does_nothing() {
    let backend = start_fixture(0).await;
    let port = backend.url().port().unwrap();
    assert_eq!(
        backend.shutdown().await,
        Some(HostShutdown {
            steps: vec![Step::ApiStopped],
            ..HostShutdown::default()
        })
    );
    assert!(backend.shutdown().await.is_none());
    // The port was the system's pick and another test can take it at once: the API must no
    // longer answer its token, which is not the same as nothing accepting.
    assert_ne!(
        puddle_test_kit::http_status(
            ([127, 0, 0, 1], port).into(),
            "/api/pending",
            backend.token()
        ),
        Some(200)
    );
}

#[tokio::test]
async fn the_fixture_writes_the_connection_file_it_is_asked_for() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("api.json");
    let options = Options {
        connection_file: Some(file.clone()),
        ..fixture_options()
    };
    let backend = Source::Fixture.start(&options).await.unwrap();
    let read = ConnectionInfo::read(&file).unwrap();
    assert_eq!(read.url, backend.url().as_str().trim_end_matches('/'));
    assert_eq!(read.token.expose(), backend.token());
    backend.shutdown().await;
    assert!(format!("{backend:?}").contains("127.0.0.1"));
    assert!(!format!("{backend:?}").contains(backend.token()));
}

#[tokio::test]
async fn two_backends_get_their_own_ports_and_tokens() {
    let a = start_fixture(0).await;
    let b = start_fixture(0).await;
    assert_ne!(a.url(), b.url());
    assert_ne!(a.token(), b.token());
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn a_taken_port_is_reported() {
    let a = start_fixture(0).await;
    let options = Options {
        port: a.url().port().unwrap(),
        ..fixture_options()
    };
    let err = Source::Fixture.start(&options).await.unwrap_err();
    assert!(matches!(err, BackendError::Serve(_)), "{err:?}");
    a.shutdown().await;
}
