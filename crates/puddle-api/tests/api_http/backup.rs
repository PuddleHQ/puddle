// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /api/backup` over real HTTP: the download's headers and body, the refusals.

use std::sync::Arc;

use puddle_services::{backup::FAKE_BACKUP_BYTES, backup::FakeBackup};

use crate::common::{raw, start, start_with_backup};

#[tokio::test]
async fn without_a_service_a_backup_says_it_is_unavailable() {
    let api = start().await;
    let reply = api.send("POST", "/api/backup", None).await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.error(), "unavailable");
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_backup_is_one_named_download_that_is_never_cached() {
    let fake = Arc::new(FakeBackup::new());
    let api = start_with_backup(fake.clone()).await;
    let reply = api.send("POST", "/api/backup", None).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.header("content-type"), Some("application/gzip"));
    assert_eq!(
        reply.header("content-disposition"),
        // The test clock starts at 2023-11-14T22:13:20Z.
        Some("attachment; filename=\"puddle-backup-20231114T221320Z.tar.gz\"")
    );
    assert_eq!(reply.header("cache-control"), Some("no-store"));
    assert_eq!(
        reply.header("content-length"),
        Some(FAKE_BACKUP_BYTES.len().to_string().as_str())
    );
    assert_eq!(reply.body.as_bytes(), FAKE_BACKUP_BYTES);
    assert_eq!(fake.requested(), 1);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_backup_that_cannot_be_made_says_why_and_sends_no_file() {
    let fake = Arc::new(FakeBackup::new());
    fake.fail_with(Some("cannot read the database: the disk is full"));
    let api = start_with_backup(fake.clone()).await;
    let reply = api.send("POST", "/api/backup", None).await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.error(), "unavailable");
    assert!(reply.body.contains("the disk is full"), "{}", reply.body);
    assert_eq!(reply.header("content-disposition"), None);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_backup_needs_the_token_and_a_get_does_not_start_one() {
    let fake = Arc::new(FakeBackup::new());
    let api = start_with_backup(fake.clone()).await;
    let request = format!(
        "POST /api/backup HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        api.host()
    );
    assert_eq!(raw(api.addr, request.as_bytes()).await.status, 401);
    let get = api.get("/api/backup").await;
    assert_ne!(get.status, 200, "a GET made a backup: {get:?}");
    assert_eq!(fake.requested(), 0, "nothing was made for either");
    api.running.shutdown().await;
}
