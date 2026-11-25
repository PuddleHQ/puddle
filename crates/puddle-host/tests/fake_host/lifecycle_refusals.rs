// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a failed boot, a dirty delete and a busy workspace refuse.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_boot_leaves_nothing_behind_and_a_failed_start_says_crashed() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("boot hook"),
        "{end}"
    );
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());

    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    // The failed create's id is retired: the next one of the name gets another.
    create_as(&api, &mut events, "acme", "acme-2").await;
    api.post("/api/workspaces/acme-2/stop", "").await;
    events.until(ended("acme-2"), Duration::from_secs(20)).await;
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    api.post("/api/workspaces/acme-2/start", "").await;
    let end = events.until(ended("acme-2"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert_eq!(
        status_of(&api.get("/api/workspaces/acme-2").await),
        "crashed"
    );
    // The volume and the workspace survive a failed start.
    assert_eq!(rig.runtime.list_volumes().await.unwrap().len(), 1);
    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    api.post("/api/workspaces/acme-2/start", "").await;
    let end = events.until(ended("acme-2"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dirty_workspace_is_deleted_only_with_the_fingerprint_the_user_saw() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    rig.guest.dirty.store(true, Ordering::SeqCst);
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    assert_eq!(check["clean"], false);
    let fingerprint = check["fingerprint"].as_str().unwrap().to_owned();

    let bare = json!({"confirm": true}).to_string();
    let refused = api.delete("/api/workspaces/acme", Some(&bare)).await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    let stale = json!({"confirm": true, "fingerprint": "0".repeat(64)}).to_string();
    assert_eq!(
        api.delete("/api/workspaces/acme", Some(&stale))
            .await
            .status,
        409
    );
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let seen = json!({"confirm": true, "fingerprint": fingerprint}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&seen)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_workspace_cannot_be_deleted_and_busy_ones_refuse_a_second_operation() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let delete = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(delete.status, 409, "{}", delete.body);
    assert_eq!(api.post("/api/workspaces/acme/start", "").await.status, 409);
    assert_eq!(
        api.post("/api/workspaces", &new_workspace("acme"))
            .await
            .status,
        409
    );
    assert_eq!(api.get("/api/workspaces/nope").await.status, 404);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn reclaim_trims_the_running_workspace() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let reply = api.post("/api/workspaces/acme/reclaim", "").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done");
    assert!(rig.guest.commands().iter().any(|c| c.ends_with(" trim")));
    host.shutdown().await;
}

/// A boot whose hook failed and stopped its sandbox leaves nothing to the lifecycle: the shutdown
/// finds no sandbox to stop.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_boot_leaves_no_sandbox_for_the_shutdown_to_stop() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");

    let down = host.shutdown().await;
    assert!(down.sandboxes.sandboxes.is_empty(), "{down:?}");
}
