// SPDX-License-Identifier: AGPL-3.0-or-later
//! The editor stream when the host's side fails: each case scripts the stream with
//! [`tokio_test::io::Builder`], which panics on a byte the agent writes that the script does not
//! expect, so "answers nothing" is checked by the stream itself.
use std::io;

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio_test::io::Builder;

use super::*;
use puddle_test_kit::capture_logs;

fn gone() -> io::Error {
    io::Error::from(io::ErrorKind::ConnectionReset)
}

fn access(port: u16, password_file: PathBuf) -> EditorAccess {
    EditorAccess {
        port,
        password_file,
        unsupported: None,
        server: None,
    }
}

#[tokio::test]
async fn a_stream_closed_before_its_request_is_dropped_without_an_answer() {
    let (log, _guard) = capture_logs();
    let host = Builder::new().build();
    serve(host, Some(&access(1, PathBuf::new()))).await;
    assert!(
        log.text().contains("sent no request in time"),
        "{}",
        log.text()
    );
}

#[tokio::test]
async fn a_stream_that_resets_before_its_request_is_dropped_without_an_answer() {
    let (log, _guard) = capture_logs();
    let host = Builder::new().read_error(gone()).build();
    serve(host, Some(&access(1, PathBuf::new()))).await;
    assert!(
        log.text().contains("sent no request in time"),
        "{}",
        log.text()
    );
}

#[tokio::test]
async fn a_request_that_is_neither_connect_nor_password_is_dropped_without_an_answer() {
    let (log, _guard) = capture_logs();
    let host = Builder::new().read(b"delete\n").build();
    serve(host, Some(&access(1, PathBuf::new()))).await;
    assert!(
        log.text().contains("unusable request was dropped"),
        "{}",
        log.text()
    );
}

#[tokio::test]
async fn a_host_stream_that_resets_mid_connection_resets_the_editor_connection() {
    let (log, _guard) = capture_logs();
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = Builder::new()
        .read(b"connect\n")
        .write(b"ok\n")
        .read_error(gone())
        .build();
    serve(host, Some(&access(port, PathBuf::new()))).await;
    let (mut accepted, _) = listener.accept().await.unwrap();
    let mut rest = Vec::new();
    let read = accepted.read_to_end(&mut rest).await;
    assert!(
        read.is_err(),
        "the editor server saw a clean end ({read:?}) after the host's stream failed"
    );
    assert!(
        log.text()
            .contains("an editor connection ended with an error"),
        "{}",
        log.text()
    );
}

#[tokio::test]
async fn a_host_stream_that_fails_at_the_answer_closes_the_editor_connection() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = Builder::new()
        .read(b"connect\n")
        .write_error(io::Error::from(io::ErrorKind::BrokenPipe))
        .build();
    serve(host, Some(&access(port, PathBuf::new()))).await;
    let (mut accepted, _) = listener.accept().await.unwrap();
    let mut rest = Vec::new();
    accepted.read_to_end(&mut rest).await.unwrap();
    assert!(rest.is_empty(), "the editor server got {rest:?}");
}

#[tokio::test]
async fn a_host_stream_that_fails_while_the_password_is_sent_ends_the_request() {
    let (log, _guard) = capture_logs();
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("password");
    std::fs::write(&file, "hunter2\n").unwrap();
    let host = Builder::new()
        .read(b"password\n")
        .write_error(io::Error::from(io::ErrorKind::BrokenPipe))
        .build();
    serve(host, Some(&access(1, file))).await;
    assert!(
        log.text().contains("the editor password could not be sent"),
        "{}",
        log.text()
    );
}
