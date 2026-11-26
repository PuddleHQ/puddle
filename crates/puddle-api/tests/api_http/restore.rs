// SPDX-License-Identifier: AGPL-3.0-or-later
//! `POST /api/restore/preview` over real HTTP: the upload, the preview, the refusals.

use std::sync::Arc;

use puddle_services::wire::{RestorePart, RestorePartName};
use puddle_services::{
    restore::FAKE_RESTORE_MAGIC, restore::FakeRestore, restore::RestoreFacts, restore::SecretRef,
};

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
        token_refs: Vec::new(),
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

#[tokio::test]
async fn a_confirm_names_the_staged_copy_and_the_mode_and_asks_for_a_restart() {
    let fake = Arc::new(FakeRestore::new());
    let api = start_with_restore(fake.clone()).await;
    let staging_id = "0".repeat(32);
    let body = serde_json::json!({"staging_id": staging_id, "mode": "replace"});
    let reply = api.send("POST", "/api/restore/apply", Some(&body)).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["restart_required"], true);
    assert_eq!(reply.json()["safety_copy"], "20231114T221320Z");
    assert_eq!(
        fake.applied(),
        [(
            staging_id.clone(),
            puddle_services::wire::RestoreMode::Replace
        )]
    );
    // A staged copy that is gone is a 404, a mode nobody offers is a 422, and an unknown field too.
    let gone = serde_json::json!({"staging_id": "1".repeat(32), "mode": "add"});
    assert_eq!(
        api.send("POST", "/api/restore/apply", Some(&gone))
            .await
            .status,
        404
    );
    let odd = serde_json::json!({"staging_id": staging_id, "mode": "merge"});
    assert_eq!(
        api.send("POST", "/api/restore/apply", Some(&odd))
            .await
            .status,
        422
    );
    let extra = serde_json::json!({"staging_id": staging_id, "mode": "add", "force": true});
    assert_eq!(
        api.send("POST", "/api/restore/apply", Some(&extra))
            .await
            .status,
        422
    );
    assert_eq!(fake.applied().len(), 1);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_confirm_that_cannot_make_its_safety_copy_is_a_503_that_says_so() {
    let fake = Arc::new(FakeRestore::new());
    fake.fail_apply_with(Some("the safety copy could not be made: the disk is full"));
    let api = start_with_restore(fake.clone()).await;
    let body = serde_json::json!({"staging_id": "0".repeat(32), "mode": "add"});
    let reply = api.send("POST", "/api/restore/apply", Some(&body)).await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("the disk is full"), "{}", reply.body);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_last_result_is_read_and_forgotten_and_safety_copies_are_listed_and_deleted() {
    use puddle_services::wire::{RestoreMode, RestoreResult, SafetyCopy};
    let fake = Arc::new(FakeRestore::new());
    fake.set_result(Some(RestoreResult {
        applied: true,
        mode: RestoreMode::Add,
        safety_copy: "20231114T221320Z".into(),
        at_ms: 5,
        message: "Restored.".into(),
    }));
    fake.set_safety_copies(vec![
        SafetyCopy {
            name: "20231114T221320Z".into(),
            size_bytes: 4096,
        },
        SafetyCopy {
            name: "20231001T000000Z".into(),
            size_bytes: 10,
        },
    ]);
    let api = start_with_restore(fake.clone()).await;
    let result = api.get("/api/restore/result").await.json();
    assert_eq!(result["result"]["mode"], "add");
    assert_eq!(result["result"]["message"], "Restored.");
    assert_eq!(
        api.send("DELETE", "/api/restore/result", None).await.status,
        204
    );
    assert_eq!(
        api.get("/api/restore/result").await.json()["result"],
        serde_json::Value::Null
    );

    let list = api.get("/api/safety-copies").await.json();
    assert_eq!(list["copies"][0]["size_bytes"], 4096);
    assert_eq!(list["copies"].as_array().unwrap().len(), 2);
    assert_eq!(
        api.send("DELETE", "/api/safety-copies/20231001T000000Z", None)
            .await
            .status,
        204
    );
    assert_eq!(
        api.send("DELETE", "/api/safety-copies/20231001T000000Z", None)
            .await
            .status,
        404
    );
    assert_eq!(
        api.get("/api/safety-copies").await.json()["copies"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_preview_says_which_disks_are_here_which_secrets_lack_a_value_and_what_runs() {
    use puddle_secrets::{Secret, SecretStore as _, StoredId};

    let fake = Arc::new(FakeRestore::new());
    let api = start_with_restore(fake.clone()).await;
    // A workspace of this machine that runs, one unclaimed disk, and secrets with and without a value.
    let made = serde_json::json!({"name": "mine", "repositories": [{"url": "https://github.com/acme/api.git"}]});
    let created = api.send("POST", "/api/workspaces", Some(&made)).await;
    assert_eq!(created.status, 202, "{}", created.body);
    api.workspaces.idle().await;
    let mine = api.get("/api/workspaces").await.json()["workspaces"][0].clone();
    let mine_id = mine["id"].as_str().unwrap().to_owned();
    let running = u64::from(matches!(
        mine["status"].as_str(),
        Some("running" | "starting")
    ));
    api.workspaces
        .add_orphan_volume(&puddle_types::WorkspaceId::new("kept").unwrap(), None);
    api.secrets
        .set(
            &StoredId::new("restore-has-value").unwrap(),
            &Secret::new("x".into()),
        )
        .unwrap();
    let mut facts = facts();
    facts.workspaces = vec![
        (mine_id.clone(), "Mine".into()),
        ("kept".into(), "Kept".into()),
        ("gone".into(), "Gone".into()),
        ("not a valid id!".into(), "Odd".into()),
    ];
    let secret = |name: &str, id: &str| SecretRef {
        workspace_id: Some("gone".into()),
        name: name.into(),
        secret_id: id.into(),
    };
    facts.secret_refs = vec![
        secret("HAS", "restore-has-value"),
        secret("LACKS", "restore-no-value"),
        secret("BADID", "not a valid id!"),
    ];
    let token = |identity: &str, id: &str| puddle_services::restore::IdentityTokenRef {
        identity: identity.into(),
        host: "github.com".into(),
        secret_id: id.into(),
    };
    api.secrets
        .set(
            &StoredId::new("restore-token-here").unwrap(),
            &Secret::new("x".into()),
        )
        .unwrap();
    facts.token_refs = vec![
        token("has", "restore-token-here"),
        token("lacks", "restore-token-lost"),
    ];
    fake.set_facts(facts);

    let reply = api
        .send_file("POST", "/api/restore/preview", FAKE_RESTORE_MAGIC)
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let json = reply.json();
    assert_eq!(
        json["identity_tokens_needing_value"],
        serde_json::json!([{"identity": "lacks", "host": "github.com"}])
    );
    let disks: Vec<(&str, &str)> = json["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["id"].as_str().unwrap(), w["disk"].as_str().unwrap()))
        .collect();
    assert_eq!(
        disks,
        [
            (mine_id.as_str(), "present"),
            ("kept", "present"),
            ("gone", "missing"),
            ("not a valid id!", "missing")
        ]
    );
    let names: Vec<&str> = json["secrets_needing_value"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["LACKS", "BADID"]);
    assert_eq!(json["running_workspaces"], running);
    api.running.shutdown().await;
}

#[tokio::test]
async fn without_a_service_every_other_restore_call_says_it_is_unavailable() {
    let api = start().await;
    let body = serde_json::json!({"staging_id": "0".repeat(32), "mode": "add"});
    for (method, path, body) in [
        ("POST", "/api/restore/apply", Some(&body)),
        ("GET", "/api/restore/result", None),
        ("DELETE", "/api/restore/result", None),
        ("GET", "/api/safety-copies", None),
        ("DELETE", "/api/safety-copies/20231114T221320Z", None),
    ] {
        let reply = api.send(method, path, body).await;
        assert_eq!(reply.status, 503, "{method} {path}: {}", reply.body);
        assert_eq!(reply.error(), "unavailable");
    }
    api.running.shutdown().await;
}
