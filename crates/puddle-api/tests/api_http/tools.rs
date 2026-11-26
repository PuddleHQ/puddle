// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace's managed tools over real HTTP against `FakeWorkspaces`: the listing, the edits of
//! the root file with the hash check, the registry, the version list and the install retry, and
//! the status of every refusal.

use puddle_services::wire::{RegistryTool, ToolEntry, ToolsFile};
use puddle_types::{ToolName, ToolVersion, WorkspaceId};
use serde_json::{Value, json};

use crate::common::{Api, Reply, start};

const REPO: &str = "https://github.com/acme/api.git";

fn name(n: &str) -> ToolName {
    ToolName::new(n).unwrap()
}

fn version(v: &str) -> ToolVersion {
    ToolVersion::new(v).unwrap()
}

fn id(n: &str) -> WorkspaceId {
    WorkspaceId::new(n).unwrap()
}

async fn workspace(api: &Api, workspace: &str, running: bool) {
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": workspace, "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    api.workspaces.idle().await;
    if running {
        let reply = api
            .send("POST", &format!("/api/workspaces/{workspace}/start"), None)
            .await;
        assert!(reply.status == 202 || reply.status == 200, "{}", reply.body);
        api.workspaces.idle().await;
    }
}

async fn tools(api: &Api) -> Value {
    let reply = api.get("/api/workspaces/shop/tools").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

async fn put(api: &Api, tool: &str, version: &str, hash: &str) -> Reply {
    api.send(
        "PUT",
        &format!("/api/workspaces/shop/tools/{tool}"),
        Some(&json!({"version": version, "sha256": hash})),
    )
    .await
}

async fn delete(api: &Api, tool: &str, hash: &str) -> Reply {
    api.send(
        "DELETE",
        &format!("/api/workspaces/shop/tools/{tool}?sha256={hash}"),
        None,
    )
    .await
}

#[tokio::test]
async fn the_listing_has_the_root_file_with_its_hash_and_the_repositories_files_after_it() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    let empty = tools(&api).await;
    assert_eq!(empty["running"], true);
    assert_eq!(empty["sha256"], "absent");
    assert_eq!(empty["files"], json!([]));
    assert!(empty["problem"].is_null());

    api.workspaces
        .set_tools(&id("shop"), vec![(name("node"), version("22"), true)]);
    api.workspaces.set_tool_files(
        &id("shop"),
        vec![ToolsFile {
            path: "api/.tool-versions".to_owned(),
            root: false,
            tools: vec![ToolEntry {
                name: name("python"),
                requested: version("3.12"),
                installed: false,
            }],
        }],
    );
    let body = tools(&api).await;
    assert!(body["listed_at"].is_u64());
    assert_eq!(body["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(
        body["files"],
        json!([
            {"path": "mise.toml", "root": true,
             "tools": [{"name": "node", "requested": "22", "installed": true}]},
            {"path": "api/.tool-versions", "root": false,
             "tools": [{"name": "python", "requested": "3.12", "installed": false}]}
        ])
    );
    let missing = api.get("/api/workspaces/nope/tools").await;
    assert_eq!(missing.status, 404, "{}", missing.body);
}

#[tokio::test]
async fn add_change_and_remove_each_answer_the_new_listing_and_start_an_install() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    let hash = tools(&api).await["sha256"].as_str().unwrap().to_owned();
    let added = put(&api, "npm:prettier", "latest", &hash).await;
    assert_eq!(added.status, 200, "{}", added.body);
    let added = added.json();
    assert_eq!(
        added["files"][0]["tools"],
        json!([{"name": "npm:prettier", "requested": "latest", "installed": false}])
    );
    let hash = added["sha256"].as_str().unwrap().to_owned();
    assert_ne!(hash, "absent");
    let changed = put(&api, "npm:prettier", "3", &hash).await.json();
    assert_eq!(changed["files"][0]["tools"][0]["requested"], "3");
    let hash = changed["sha256"].as_str().unwrap().to_owned();
    let removed = delete(&api, "npm:prettier", &hash).await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    assert_eq!(removed.json()["files"], json!([]));
    assert_eq!(api.workspaces.tool_installs(), vec![id("shop"); 3]);
}

#[tokio::test]
async fn a_file_changed_by_hand_since_the_listing_answers_409_and_changes_nothing() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    api.workspaces
        .set_tools(&id("shop"), vec![(name("node"), version("22"), true)]);
    let hash = tools(&api).await["sha256"].as_str().unwrap().to_owned();
    api.workspaces.edit_tools_file_by_hand(&id("shop"));
    for reply in [
        put(&api, "go", "1.23", &hash).await,
        delete(&api, "node", &hash).await,
    ] {
        assert_eq!(reply.status, 409, "{}", reply.body);
        assert!(
            reply.body.contains("changed since it was listed"),
            "{}",
            reply.body
        );
    }
    assert_eq!(api.workspaces.tool_installs(), Vec::<WorkspaceId>::new());
    let after = tools(&api).await;
    assert_eq!(after["files"][0]["tools"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn a_bad_name_version_or_hash_answers_422_before_anything_runs() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    let hash = "a".repeat(64);
    for reply in [
        put(&api, "a%20b", "22", &hash).await,
        put(&api, "node", "--yes", &hash).await,
        put(&api, "node", "22", "ABC").await,
        delete(&api, "-x", &hash).await,
        delete(&api, "node", "zz").await,
        api.get("/api/workspaces/shop/tools/-x/versions").await,
    ] {
        assert_eq!(reply.status, 422, "{}", reply.body);
    }
    let no_hash = api
        .send("DELETE", "/api/workspaces/shop/tools/node", None)
        .await;
    assert_eq!(no_hash.status, 400, "{}", no_hash.body);
    assert_eq!(api.workspaces.tool_installs(), Vec::<WorkspaceId>::new());
}

#[tokio::test]
async fn a_stopped_workspace_answers_its_last_listing_and_refuses_edits_and_lookups() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    api.workspaces
        .set_tools(&id("shop"), vec![(name("node"), version("22"), true)]);
    let live = tools(&api).await;
    let stop = api.send("POST", "/api/workspaces/shop/stop", None).await;
    assert!(stop.status == 202 || stop.status == 200, "{}", stop.body);
    api.workspaces.idle().await;
    let stopped = tools(&api).await;
    assert_eq!(stopped["running"], false);
    assert_eq!(stopped["listed_at"], live["listed_at"]);
    assert_eq!(stopped["files"], live["files"]);
    assert!(stopped["problem"].as_str().unwrap().contains("start it"));
    for reply in [
        put(&api, "go", "1", "absent").await,
        delete(&api, "node", "absent").await,
        api.get("/api/workspaces/shop/tools/registry").await,
        api.get("/api/workspaces/shop/tools/node/versions").await,
        api.send("POST", "/api/workspaces/shop/tools/install", None)
            .await,
    ] {
        assert_eq!(reply.status, 409, "{}", reply.body);
    }
}

#[tokio::test]
async fn with_tools_off_the_listing_says_so_and_every_call_that_needs_them_answers_409() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    api.workspaces.set_tools_off(&id("shop"), true);
    let body = tools(&api).await;
    assert_eq!(body["problem"], "tools are off for this workspace");
    assert!(body["sha256"].is_null() && body["files"] == json!([]));
    let reply = put(&api, "go", "1", "absent").await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    api.workspaces.set_tools_off(&id("shop"), false);
    assert_eq!(put(&api, "go", "1", "absent").await.status, 200);
}

#[tokio::test]
async fn the_registry_and_the_versions_come_with_a_reason_when_mise_could_not_say() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    api.workspaces.set_tool_registry(
        vec![RegistryTool {
            name: name("node"),
            description: "JavaScript runtime".to_owned(),
        }],
        None,
    );
    let registry = api.get("/api/workspaces/shop/tools/registry").await;
    assert_eq!(registry.status, 200, "{}", registry.body);
    assert_eq!(
        registry.json(),
        json!({"tools": [{"name": "node", "description": "JavaScript runtime"}], "problem": null})
    );
    api.workspaces
        .set_tool_versions(name("node"), Ok(vec![version("22.1.0"), version("22.0.0")]));
    api.workspaces.set_tool_versions(
        name("go"),
        Err("mise ERROR no route to mise-versions.jdx.dev".to_owned()),
    );
    let node = api.get("/api/workspaces/shop/tools/node/versions").await;
    assert_eq!(
        node.json(),
        json!({"versions": ["22.1.0", "22.0.0"], "reason": null})
    );
    let go = api.get("/api/workspaces/shop/tools/go/versions").await;
    assert_eq!(
        go.json(),
        json!({"versions": [], "reason": "mise ERROR no route to mise-versions.jdx.dev"})
    );
    let unknown = api.get("/api/workspaces/shop/tools/zig/versions").await;
    assert_eq!(unknown.json(), json!({"versions": [], "reason": null}));
}

#[tokio::test]
async fn the_install_retry_is_accepted_and_runs_for_the_workspace() {
    let api = start().await;
    workspace(&api, "shop", true).await;
    let reply = api
        .send("POST", "/api/workspaces/shop/tools/install", None)
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(api.workspaces.tool_installs(), vec![id("shop")]);
}
