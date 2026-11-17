// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /api/restore/preview` over real HTTP: the upload, the preview, the refusals.

use std::sync::Arc;

use puddle_api::wire::{RestorePart, RestorePartName};
use puddle_api::{FAKE_RESTORE_MAGIC, FakeRestore, RestoreFacts, SecretRef};

use crate::common::{raw, start, start_with_restore};

fn facts() -> RestoreFacts {
    RestoreFacts {
        staging_id: "ab".repeat(16),
        format: 1,
        puddle_version: Some("0.1.0".into()),
        schema_version: 32,
        created_at_ms: Some(1_700_000_000_000),
        parts: vec![RestorePart {
            name: RestorePartName::Rules,
            in_backup: 5,
            add: 2,
            differ: 1,
            same: 2,
            only_here: 3,
        }],
        workspaces: vec![("w-gone".into(), "Gone".into())],
        secret_refs: vec![SecretRef {
            workspace_id: None,
            name: "TOKEN".into(),
            secret_id: "env-no-such-value".into(),
        }],
    }
}

#[tokio::test]
async fn without_a_service_a_preview_says_it_is_unavailable() {
    let api = start().await;
    let reply = api.send_file("POST", "/api/restore/preview", b"x").await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(reply.error(), "unavailable");
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_preview_carries_the_service_facts_and_what_only_the_api_knows() {
    let fake = Arc::new(FakeRestore::new());
    fake.set_facts(facts());
    let api = start_with_restore(fake.clone()).await;
    // A body far above the usual 64 KiB limit goes through.
    let mut body = FAKE_RESTORE_MAGIC.to_vec();
    body.resize(300 * 1024, b'.');
    let reply = api.send_file("POST", "/api/restore/preview", &body).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let json = reply.json();
    assert_eq!(json["staging_id"], "ab".repeat(16));
    assert_eq!(json["format"], 1);
    assert_eq!(json["parts"][0]["name"], "rules");
    assert_eq!(json["parts"][0]["differ"], 1);
    assert_eq!(json["parts"][0]["only_here"], 3);
    // The restored workspace has no disk here, and its secret has no value here.
    assert_eq!(json["workspaces"][0]["id"], "w-gone");
    assert_eq!(json["workspaces"][0]["disk"], "missing");
    assert_eq!(json["secrets_needing_value"][0]["name"], "TOKEN");
    assert_eq!(json["running_workspaces"], 0);
    assert_eq!(fake.last_upload_bytes(), body.len());
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_file_that_is_not_a_backup_is_a_422_with_the_reason() {
    let fake = Arc::new(FakeRestore::new());
    let api = start_with_restore(fake.clone()).await;
    let reply = api
        .send_file("POST", "/api/restore/preview", b"hello, not a backup")
        .await;
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert_eq!(reply.error(), "invalid");
    assert!(reply.body.contains("not a gzip archive"), "{}", reply.body);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_preview_needs_the_token_and_a_get_does_not_start_one() {
    let fake = Arc::new(FakeRestore::new());
    let api = start_with_restore(fake.clone()).await;
    let request = format!(
        "POST /api/restore/preview HTTP/1.1\r\nHost: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        api.host()
    );
    assert_eq!(raw(api.addr, request.as_bytes()).await.status, 401);
    let get = api.get("/api/restore/preview").await;
    assert_ne!(get.status, 200, "{get:?}");
    assert_eq!(fake.previews(), 0);
    api.running.shutdown().await;
}
