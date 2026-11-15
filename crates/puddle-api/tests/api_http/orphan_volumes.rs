// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workspace disks the workspace list does not name: listing them and restoring one over real
//! HTTP.

use puddle_api::RepoUrl;
use puddle_types::WorkspaceId;
use serde_json::json;

use crate::common::start;

const REPO: &str = "https://github.com/acme/api.git";

fn id(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

#[tokio::test]
async fn an_unclaimed_disk_is_listed_and_a_restore_makes_a_stopped_workspace_of_it() {
    let api = start().await;
    assert_eq!(
        api.get("/api/orphan-volumes").await.json(),
        json!({"volumes": []})
    );
    api.workspaces
        .add_orphan_volume(&id("lost"), Some(RepoUrl::parse(REPO).unwrap()));
    let listed = api.get("/api/orphan-volumes").await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    assert_eq!(
        listed.json(),
        json!({"volumes": [{"id": "lost", "volume": "ws-lost"}]})
    );

    let restored = api
        .send("POST", "/api/orphan-volumes/lost/restore", Some(&json!({})))
        .await;
    assert_eq!(restored.status, 201, "{}", restored.body);
    let workspace = restored.json();
    assert_eq!(workspace["name"], "lost");
    assert_eq!(workspace["status"], "stopped");
    assert_eq!(workspace["repositories"][0]["url"], REPO);

    // It is in the list now, and the disk left the notice.
    let all = api.get("/api/workspaces").await.json();
    assert_eq!(all["workspaces"][0]["name"], "lost", "{all}");
    assert_eq!(
        api.get("/api/orphan-volumes").await.json(),
        json!({"volumes": []})
    );
}

#[tokio::test]
async fn a_disk_with_no_readable_address_asks_for_one_and_refuses_one_that_is_not_https() {
    let api = start().await;
    api.workspaces.add_orphan_volume(&id("bare"), None);

    let asked = api
        .send("POST", "/api/orphan-volumes/bare/restore", Some(&json!({})))
        .await;
    assert_eq!(asked.status, 422, "{}", asked.body);
    assert!(asked.body.contains("enter the address"), "{}", asked.body);
    let ssh = api
        .send(
            "POST",
            "/api/orphan-volumes/bare/restore",
            Some(&json!({"repository": "git@github.com:acme/api.git"})),
        )
        .await;
    assert_eq!(ssh.status, 422, "{}", ssh.body);
    // Neither attempt used up the disk.
    assert_eq!(
        api.get("/api/orphan-volumes").await.json()["volumes"][0]["id"],
        "bare"
    );

    let given = api
        .send(
            "POST",
            "/api/orphan-volumes/bare/restore",
            Some(&json!({"repository": REPO})),
        )
        .await;
    assert_eq!(given.status, 201, "{}", given.body);
    assert_eq!(given.json()["repositories"][0]["directory"], "api");
}

#[tokio::test]
async fn a_restore_never_replaces_an_entry_and_an_unknown_disk_is_not_found() {
    let api = start().await;
    let made = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "shop", "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(made.status, 202, "{}", made.body);
    api.workspaces.idle().await;
    api.workspaces
        .add_orphan_volume(&id("shop"), Some(RepoUrl::parse(REPO).unwrap()));

    let clash = api
        .send("POST", "/api/orphan-volumes/shop/restore", Some(&json!({})))
        .await;
    assert_eq!(clash.status, 409, "{}", clash.body);
    assert!(
        clash.body.contains("nothing was replaced"),
        "{}",
        clash.body
    );

    let nothing = api
        .send("POST", "/api/orphan-volumes/gone/restore", Some(&json!({})))
        .await;
    assert_eq!(nothing.status, 404, "{}", nothing.body);
}
