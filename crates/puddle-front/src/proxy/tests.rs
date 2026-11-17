// SPDX-License-Identifier: AGPL-3.0-or-later
//! An upgraded editor socket whose either side fails. Each side is scripted with
//! [`tokio_test::io::Builder`], which can end a read with an error at a chosen point.
use std::io;

use puddle_test_kit::capture_logs;
use tokio_test::io::Builder;

use super::*;
use crate::windows::Windows;

fn reset() -> io::Error {
    io::Error::from(io::ErrorKind::ConnectionReset)
}

fn workspace() -> WorkspaceId {
    WorkspaceId::new("box").unwrap()
}

#[tokio::test]
async fn a_client_that_resets_ends_the_socket_and_closes_its_window() {
    let (log, _guard) = capture_logs();
    let windows = Windows::default();
    let client = Builder::new().read(b"hello").read_error(reset()).build();
    let server = Builder::new().write(b"hello").build();
    carry(client, server, Some(windows.open(&workspace()))).await;
    assert!(
        log.text().contains("an editor socket ended with an error"),
        "{}",
        log.text()
    );
    assert_eq!(windows.connected(&workspace()), 0);
}

#[tokio::test]
async fn a_server_that_resets_ends_the_socket_after_passing_on_what_it_sent() {
    let (log, _guard) = capture_logs();
    let client = Builder::new().write(b"hello").build();
    let server = Builder::new().read(b"hello").read_error(reset()).build();
    carry(client, server, None).await;
    assert!(
        log.text().contains("an editor socket ended with an error"),
        "{}",
        log.text()
    );
}
