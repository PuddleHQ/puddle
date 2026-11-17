// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restore from a backup file on the whole host: preview, confirm, the stop that applies it, the
//! apply at the next start, and the safety copy.

use super::*;

async fn add_rule(api: &Api, host: &str) {
    let body = json!({"scope": {"type": "global"}, "pattern": host, "effect": "allow"});
    let reply = api.post("/api/rules", &body.to_string()).await;
    assert_eq!(reply.status, 201, "{}", reply.body);
}

fn patterns(api_reply: &support::Reply) -> Vec<String> {
    let mut found: Vec<String> = api_reply.json()["rules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["pattern"].as_str().unwrap().to_owned())
        .collect();
    found.sort();
    found
}

/// A backup of a machine that has one rule, `source.example.org`.
async fn source_backup() -> Vec<u8> {
    let source = Rig::new();
    let host = source.start().await;
    add_rule(&api(&host), "source.example.org").await;
    host.shutdown().await;
    drop(host);
    let mut archive =
        puddle_host::backup_archive(&HostPaths::new(source.dir.path().join("data"))).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut archive.file, &mut bytes).unwrap();
    bytes
}

#[tokio::test(flavor = "multi_thread")]
async fn a_confirmed_restore_applies_at_the_next_start_and_leaves_a_safety_copy() {
    let backup = source_backup().await;
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    add_rule(&api1, "mine.example.org").await;

    // Choosing the file changes nothing.
    let preview = api1.post_bytes("/api/restore/preview", &backup).await;
    assert_eq!(preview.status, 200, "{}", preview.body);
    let preview = preview.json();
    let rules = preview["parts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "rules")
        .unwrap()
        .clone();
    assert_eq!(
        (rules["add"].as_u64(), rules["only_here"].as_u64()),
        (Some(1), Some(1)),
        "{preview}"
    );
    assert_eq!(
        patterns(&api1.get("/api/rules").await),
        ["mine.example.org"]
    );

    // Confirming asks the process to stop; the apply waits for the next start.
    let applied = api1
        .post(
            "/api/restore/apply",
            &json!({"staging_id": preview["staging_id"], "mode": "add"}).to_string(),
        )
        .await;
    assert_eq!(applied.status, 200, "{}", applied.body);
    let safety = applied.json()["safety_copy"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(5), host.restore_requested())
        .await
        .expect("the host is told a restore is waiting");
    assert_eq!(
        patterns(&api1.get("/api/rules").await),
        ["mine.example.org"],
        "nothing changed yet"
    );
    host.shutdown().await;
    drop(host);
    drop(api1);

    let again = rig.start().await;
    let api2 = api(&again);
    assert_eq!(
        patterns(&api2.get("/api/rules").await),
        ["mine.example.org", "source.example.org"]
    );
    let result = api2.get("/api/restore/result").await.json();
    assert_eq!(result["result"]["applied"], true, "{result}");
    assert_eq!(result["result"]["safety_copy"], safety);
    let copies = api2.get("/api/safety-copies").await.json();
    assert_eq!(copies["copies"][0]["name"], safety, "{copies}");
    assert!(copies["copies"][0]["size_bytes"].as_u64().unwrap() > 0);

    // The user has seen it; the safety copy stays until the user deletes it.
    assert_eq!(api2.delete("/api/restore/result", None).await.status, 204);
    assert_eq!(
        api2.get("/api/restore/result").await.json()["result"],
        serde_json::Value::Null
    );
    assert_eq!(
        api2.delete(&format!("/api/safety-copies/{safety}"), None)
            .await
            .status,
        204
    );
    assert_eq!(
        api2.get("/api/safety-copies").await.json()["copies"],
        json!([])
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_is_not_a_backup_is_refused_and_a_replace_drops_what_the_backup_lacks() {
    let backup = source_backup().await;
    let rig = Rig::new();
    let host = rig.start().await;
    let api1 = api(&host);
    add_rule(&api1, "mine.example.org").await;
    let refused = api1
        .post_bytes("/api/restore/preview", b"not a backup")
        .await;
    assert_eq!(refused.status, 422, "{}", refused.body);
    assert!(refused.body.contains("not a gzip"), "{}", refused.body);

    let preview = api1
        .post_bytes("/api/restore/preview", &backup)
        .await
        .json();
    let applied = api1
        .post(
            "/api/restore/apply",
            &json!({"staging_id": preview["staging_id"], "mode": "replace"}).to_string(),
        )
        .await;
    assert_eq!(applied.status, 200, "{}", applied.body);
    host.shutdown().await;
    drop(host);
    drop(api1);

    let again = rig.start().await;
    assert_eq!(
        patterns(&api(&again).get("/api/rules").await),
        ["source.example.org"]
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unconfirmed_upload_does_not_outlive_a_start() {
    let backup = source_backup().await;
    let rig = Rig::new();
    let host = rig.start().await;
    let preview = api(&host)
        .post_bytes("/api/restore/preview", &backup)
        .await
        .json();
    let staged = rig
        .dir
        .path()
        .join("data")
        .join("restore-staging")
        .join(preview["staging_id"].as_str().unwrap());
    assert!(staged.is_dir());
    host.shutdown().await;
    drop(host);
    let again = rig.start().await;
    assert!(!staged.exists());
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn only_a_staged_copy_can_be_confirmed() {
    let rig = Rig::new();
    let host = rig.start().await;
    let missing = api(&host)
        .post(
            "/api/restore/apply",
            &json!({"staging_id": "0".repeat(32), "mode": "add"}).to_string(),
        )
        .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    host.shutdown().await;
}
