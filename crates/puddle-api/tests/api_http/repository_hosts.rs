// SPDX-License-Identifier: GPL-3.0-or-later
//! Creating a workspace allows the host of its repositories (storage ST-44a), over real HTTP.

use puddle_api::{DisplayName, RepoUrl, Repository, Unsaved, WorkspaceRecord};
use puddle_types::{CheckoutDir, EntryId, WorkspaceId, WorkspaceStatus};
use serde_json::{Value, json};

use crate::common::{Api, start};

const REPO: &str = "https://github.com/acme/api.git";

async fn hosts(api: &Api, urls: &[&str], workspace: Option<&str>) -> Value {
    let mut body = json!({"urls": urls});
    if let Some(w) = workspace {
        body["workspace"] = json!(w);
    }
    let reply = api.send("POST", "/api/repository-hosts", Some(&body)).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

async fn create(api: &Api, name: &str, urls: &[&str]) -> crate::common::Reply {
    let repositories: Vec<Value> = urls.iter().map(|u| json!({"url": u})).collect();
    api.send(
        "POST",
        "/api/workspaces",
        Some(&json!({"name": name, "repositories": repositories})),
    )
    .await
}

async fn rules(api: &Api) -> Vec<Value> {
    api.get("/api/rules").await.json()["rules"]
        .as_array()
        .unwrap()
        .clone()
}

async fn block(api: &Api, scope: Value, pattern: &str) -> i64 {
    let reply = api
        .send(
            "POST",
            "/api/rules",
            Some(&json!({"scope": scope, "pattern": pattern, "effect": "deny"})),
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    reply.json()["id"].as_i64().unwrap()
}

#[tokio::test]
async fn the_form_lists_each_host_once_as_new_and_skips_what_is_no_repository() {
    let api = start().await;
    let answer = hosts(
        &api,
        &[
            REPO,
            "https://github.com/acme/web",
            "git@github.com:acme/x.git",
            "nonsense",
        ],
        None,
    )
    .await;
    assert_eq!(
        answer,
        json!({"hosts": [{"host": "github.com", "state": "new", "blocked_by": null}]})
    );
    assert!(rules(&api).await.is_empty(), "asking makes no rule");
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_host_a_rule_allows_is_not_listed_as_new() {
    let api = start().await;
    let reply = api
        .send(
            "POST",
            "/api/rules",
            Some(&json!({"scope": {"type": "global"}, "pattern": "github.com", "effect": "allow"})),
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    let answer = hosts(&api, &[REPO], None).await;
    assert_eq!(answer["hosts"][0]["state"], "allowed");
    let made = create(&api, "web", &[REPO]).await;
    assert_eq!(made.status, 202, "{}", made.body);
    assert_eq!(made.json()["allowed_hosts"], json!([]));
    assert_eq!(rules(&api).await.len(), 1, "the create added no rule");
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_create_allows_the_host_for_that_workspace_as_a_rule_marked_as_made_by_the_create() {
    let api = start().await;
    let made = create(&api, "web", &[REPO]).await;
    assert_eq!(made.status, 202, "{}", made.body);
    let allowed = made.json()["allowed_hosts"].clone();
    assert_eq!(allowed[0]["host"], "github.com");
    let rules = rules(&api).await;
    assert_eq!(rules.len(), 1);
    let rule = &rules[0];
    assert_eq!(rule["pattern"], "github.com");
    assert_eq!(rule["effect"], "allow");
    assert_eq!(rule["created_by"], "workspace_create");
    assert_eq!(rule["scope"]["type"], "workspace");
    assert_eq!(rule["id"], allowed[0]["rule_id"]);
    // The host is allowed now, so the form no longer lists it as new for that workspace.
    let id = made.json()["id"].as_str().unwrap().to_owned();
    let again = hosts(&api, &[REPO], Some(&id)).await;
    assert_eq!(again["hosts"][0]["state"], "allowed");
    // A new workspace is another one: the host is new for it again.
    let other = hosts(&api, &[REPO], None).await;
    assert_eq!(other["hosts"][0]["state"], "new");
    api.workspaces.idle().await;
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_block_rule_refuses_the_create_naming_the_host_and_the_rule_and_it_makes_nothing() {
    let api = start().await;
    let id = block(&api, json!({"type": "global"}), "github.com").await;
    let answer = hosts(&api, &[REPO], None).await;
    assert_eq!(answer["hosts"][0]["state"], "blocked");
    let why = answer["hosts"][0]["blocked_by"].as_str().unwrap();
    assert!(
        why.contains("github.com") && why.contains(&format!("rule {id}")),
        "{why}"
    );
    let refused = create(&api, "web", &[REPO]).await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(
        refused.body.contains("github.com") && refused.body.contains(&format!("rule {id}")),
        "{}",
        refused.body
    );
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"],
        json!([])
    );
    assert_eq!(rules(&api).await.len(), 1, "only the block");
    api.running.shutdown().await;
}

#[tokio::test]
async fn adding_a_repository_allows_its_host_for_that_workspace_and_a_block_refuses_it() {
    let api = start().await;
    api.own("web");
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new("web").unwrap(),
        DisplayName::parse("web").unwrap(),
        vec![Repository {
            entry: EntryId::from_random([1, 2, 3, 4, 5]),
            url: RepoUrl::parse(REPO).unwrap(),
            directory: CheckoutDir::new("api").unwrap(),
        }],
    );
    record.status = WorkspaceStatus::Running;
    api.workspaces.seed(record, Unsaved::default());
    let gitea = "https://gitea.example/acme/site";
    let added = api
        .send(
            "POST",
            "/api/workspaces/web/repositories",
            Some(&json!({"kind": "clone", "url": gitea, "directory": "site"})),
        )
        .await;
    assert_eq!(added.status, 202, "{}", added.body);
    assert_eq!(added.json()["allowed_hosts"][0]["host"], "gitea.example");
    api.workspaces.idle().await;
    block(
        &api,
        json!({"type": "workspace", "workspace": "web"}),
        "gitlab.com",
    )
    .await;
    let refused = api
        .send(
            "POST",
            "/api/workspaces/web/repositories",
            Some(&json!({"kind": "clone", "url": "https://gitlab.com/acme/x", "directory": "x"})),
        )
        .await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(refused.body.contains("gitlab.com"), "{}", refused.body);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_contract_lists_the_host_check() {
    let api = start().await;
    let spec = api.get("/api/openapi.json").await.json();
    assert!(spec["paths"]["/api/repository-hosts"]["post"].is_object());
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_rule_set_that_denies_the_host_blocks_it_by_name() {
    let api = start().await;
    let set = api
        .send("POST", "/api/rule-sets", Some(&json!({"name": "Client X"})))
        .await
        .json();
    let number: i64 = set["id"]
        .as_str()
        .unwrap()
        .strip_prefix("user:")
        .unwrap()
        .parse()
        .unwrap();
    block(&api, json!({"type": "set", "set": number}), "github.com").await;
    let answer = hosts(&api, &[REPO], None).await;
    assert_eq!(answer["hosts"][0]["state"], "blocked");
    assert_eq!(
        answer["hosts"][0]["blocked_by"],
        "the rule set Client X blocks github.com"
    );
    assert_eq!(create(&api, "web", &[REPO]).await.status, 409);
    api.running.shutdown().await;
}

#[tokio::test]
async fn more_addresses_than_a_workspace_may_hold_are_refused() {
    let api = start().await;
    let urls: Vec<String> = (0..101)
        .map(|n| format!("https://github.com/acme/r{n}.git"))
        .collect();
    let reply = api
        .send(
            "POST",
            "/api/repository-hosts",
            Some(&json!({"urls": urls})),
        )
        .await;
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(
        reply.body.contains("too many repository addresses"),
        "{}",
        reply.body
    );
    api.running.shutdown().await;
}
