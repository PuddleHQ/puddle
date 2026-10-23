// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's checkouts and the calls that add, manage and remove its repositories, over real
//! HTTP against `FakeWorkspaces`: every refusal of API-56 and what API-57 lists.

use puddle_api::{Found, FoundList, RemoteNote, RepoUrl, Repository, Unsaved, WorkspaceRecord};
use puddle_types::{CheckoutDir, WorkspaceId, WorkspaceName, WorkspaceStatus};
use serde_json::{Value, json};

use crate::common::{Api, start};
use crate::events::Stream;

const API: &str = "https://github.com/acme/api.git";

fn id(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

fn found(directory: &str, remote: Option<&str>, note: Option<RemoteNote>) -> Found {
    Found {
        directory: directory.to_owned(),
        remote_url: remote.map(str::to_owned),
        remote_note: note,
        branch: Some("main".to_owned()),
        head: Some("abc1234".to_owned()),
        dirty: false,
        unreadable: None,
    }
}

/// A workspace with the entries `dirs` (`(directory, url)`), in `status`, with `guest` as what its
/// disk holds.
fn seed(api: &Api, name: &str, status: WorkspaceStatus, dirs: &[(&str, &str)], guest: Vec<Found>) {
    let entries = dirs
        .iter()
        .map(|(dir, url)| Repository {
            url: RepoUrl::parse(url).unwrap(),
            directory: CheckoutDir::new(dir).unwrap(),
        })
        .collect();
    let mut record = WorkspaceRecord::new(id(name), WorkspaceName::new(name).unwrap(), entries);
    record.status = status;
    api.workspaces.seed(record, Unsaved::default());
    api.workspaces.set_checkouts(
        &id(name),
        FoundList {
            checkouts: guest,
            more: 0,
        },
    );
}

async fn post(api: &Api, path: &str, body: &Value) -> crate::common::Reply {
    api.send("POST", path, Some(body)).await
}

async fn add(api: &Api, name: &str, body: Value) -> crate::common::Reply {
    post(api, &format!("/api/workspaces/{name}/repositories"), &body).await
}

fn running(api: &Api) {
    seed(
        api,
        "web",
        WorkspaceStatus::Running,
        &[("api", API)],
        vec![found("api", Some(API), None)],
    );
}

#[tokio::test]
async fn the_checkouts_of_a_running_workspace_show_entries_reasons_and_missing_ones() {
    let api = start().await;
    seed(
        &api,
        "web",
        WorkspaceStatus::Running,
        &[("api", API), ("gone", "https://github.com/acme/gone")],
        vec![
            found("api", Some(API), None),
            found(
                "raw",
                Some("https://github.com/acme/raw"),
                Some(RemoteNote::CredentialsRemoved),
            ),
            found("ssh", None, Some(RemoteNote::Ssh)),
            found("same", Some("https://github.com/Acme/API"), None),
        ],
    );
    let reply = api.get("/api/workspaces/web/checkouts").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let body = reply.json();
    assert_eq!(body["running"], true);
    assert!(body["checked_at"].is_u64() && body["problem"].is_null());
    assert_eq!(
        (body["more"].clone(), body["missing"].clone()),
        (json!(0), json!(["gone"]))
    );
    let by = |dir: &str| {
        body["checkouts"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["directory"] == dir)
            .unwrap()
            .clone()
    };
    assert_eq!(by("api")["entry"], json!({"url": API, "directory": "api"}));
    assert_eq!(
        by("api")["manage"],
        json!({"offered": false, "reason": "this checkout is already on the list"})
    );
    assert_eq!(by("raw")["entry"], Value::Null);
    assert_eq!(
        by("raw")["manage"],
        json!({"offered": true, "reason": null})
    );
    assert_eq!(by("raw")["remote_note"], "credentials_removed");
    assert_eq!(by("ssh")["remote_url"], Value::Null);
    assert_eq!(by("ssh")["manage"]["reason"], "SSH is not supported yet");
    assert!(
        by("same")["manage"]["reason"]
            .as_str()
            .unwrap()
            .contains("already on the list, in api"),
        "{}",
        by("same")
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_stopped_workspace_answers_its_last_listing_or_says_to_start_it() {
    let api = start().await;
    seed(
        &api,
        "web",
        WorkspaceStatus::Running,
        &[("api", API)],
        vec![found("api", Some(API), None)],
    );
    let first = api.get("/api/workspaces/web/checkouts").await.json();
    assert_eq!(first["running"], true);
    assert_eq!(
        post(&api, "/api/workspaces/web/stop", &json!({}))
            .await
            .status,
        202
    );
    api.workspaces.idle().await;
    let stopped = api.get("/api/workspaces/web/checkouts").await.json();
    assert_eq!(stopped["running"], false);
    assert_eq!(
        stopped["checked_at"], first["checked_at"],
        "the listing of this run"
    );
    assert_eq!(stopped["checkouts"], first["checkouts"]);
    assert!(
        stopped["problem"].as_str().unwrap().contains("start it"),
        "{stopped}"
    );

    seed(&api, "never", WorkspaceStatus::Stopped, &[], vec![]);
    let none = api.get("/api/workspaces/never/checkouts").await.json();
    assert_eq!(
        (none["checked_at"].clone(), none["checkouts"].clone()),
        (Value::Null, json!([]))
    );
    assert_eq!(api.get("/api/workspaces/nope/checkouts").await.status, 404);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_clone_is_accepted_busy_cloning_and_lists_the_entry_with_progress() {
    let api = start().await;
    running(&api);
    let mut stream = Stream::open(&api, "?workspace=web").await;
    api.workspaces.hold();
    let reply = add(
        &api,
        "web",
        json!({"kind": "clone", "url": "https://github.com/acme/web", "directory": "site"}),
    )
    .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let ws = reply.json();
    assert_eq!(ws["busy"], "cloning");
    assert_eq!(
        ws["repositories"],
        json!([
            {"url": API, "directory": "api"},
            {"url": "https://github.com/acme/web", "directory": "site"}
        ])
    );
    assert_eq!(
        ws["identities"].as_array().unwrap().len(),
        1,
        "one answer for the one repository"
    );
    // Stop, delete and reclaim are refused until it ends; rename is not.
    for path in ["stop", "reclaim"] {
        let r = post(&api, &format!("/api/workspaces/web/{path}"), &json!({})).await;
        assert_eq!(r.status, 409, "{path}: {}", r.body);
        assert!(r.body.contains("cloning"), "{}", r.body);
    }
    assert_eq!(
        api.send(
            "DELETE",
            "/api/workspaces/web",
            Some(&json!({"confirm": true}))
        )
        .await
        .status,
        409
    );
    let renamed = api
        .send(
            "PATCH",
            "/api/workspaces/web",
            Some(&json!({"display_name": "Website"})),
        )
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    api.workspaces.release();
    api.workspaces.idle().await;
    stream.read_until(|b| b.contains(r#""step":"done""#)).await;
    let events = stream.data();
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "workspace_repositories_changed" && e["workspace"] == "web"),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| e["type"] == "workspace_progress"
            && e["step"] == "cloning"
            && e["repository"] == "site"),
        "{events:?}"
    );
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["busy"],
        Value::Null
    );
    let after = api.get("/api/workspaces/web/checkouts").await.json();
    assert!(after["checkouts"].as_array().unwrap().iter().any(|c| c["directory"] == "site" && c["entry"]["url"] == "https://github.com/acme/web"), "{after}");
    api.running.shutdown().await;
}

/// What the workspace and its list refuse: 404 and 409.
#[tokio::test]
#[expect(clippy::too_many_lines, reason = "one table of refusals")]
async fn adding_a_repository_the_workspace_cannot_take_is_a_404_or_a_409_with_its_reason() {
    let api = start().await;
    seed(
        &api,
        "web",
        WorkspaceStatus::Running,
        &[("api", API)],
        vec![
            found("api", Some(API), None),
            found("taken", None, Some(RemoteNote::NoRemote)),
            found("ssh", None, Some(RemoteNote::Ssh)),
            found("http", None, Some(RemoteNote::NotHttps)),
        ],
    );
    seed(&api, "off", WorkspaceStatus::Stopped, &[], vec![]);
    for (workspace, body, status, part) in [
        (
            "nope",
            json!({"kind": "clone", "url": "https://github.com/a/b"}),
            404,
            "no workspace",
        ),
        (
            "off",
            json!({"kind": "clone", "url": "https://github.com/a/b"}),
            409,
            "start it",
        ),
        (
            "off",
            json!({"kind": "existing", "directory": "x"}),
            409,
            "start it",
        ),
        (
            "web",
            json!({"kind": "clone", "url": "https://github.com/a/b", "directory": "api"}),
            409,
            "already on the list",
        ),
        (
            "web",
            json!({"kind": "clone", "url": "https://github.com/ACME/api", "directory": "other"}),
            409,
            "already on the list, in api",
        ),
        (
            "web",
            json!({"kind": "clone", "url": "https://github.com/a/taken"}),
            409,
            "already exists",
        ),
        (
            "web",
            json!({"kind": "existing", "directory": "missing"}),
            409,
            "not a checkout",
        ),
        (
            "web",
            json!({"kind": "existing", "directory": "taken"}),
            409,
            "no remote",
        ),
        (
            "web",
            json!({"kind": "existing", "directory": "ssh"}),
            409,
            "SSH is not supported",
        ),
        (
            "web",
            json!({"kind": "existing", "directory": "http"}),
            409,
            "not an https://",
        ),
        (
            "web",
            json!({"kind": "existing", "directory": "api"}),
            409,
            "already on the list",
        ),
    ] {
        let reply = add(&api, workspace, body.clone()).await;
        assert_eq!(reply.status, status, "{workspace} {body}: {}", reply.body);
        assert!(
            reply.body.contains(part),
            "{workspace} {body}: {}",
            reply.body
        );
        assert!(
            !reply.body.contains("u:p@"),
            "a credential came back: {}",
            reply.body
        );
    }
    // Nothing was added by any of those.
    let ws = api.get("/api/workspaces/web").await.json();
    assert_eq!(
        ws["repositories"],
        json!([{"url": API, "directory": "api"}])
    );
    api.running.shutdown().await;
}

/// What the body gets wrong: 422.
#[tokio::test]
async fn adding_a_repository_that_makes_no_sense_is_a_422_that_never_echoes_a_credential() {
    let api = start().await;
    seed(
        &api,
        "web",
        WorkspaceStatus::Running,
        &[("api", API)],
        vec![
            found("api", Some(API), None),
            found("taken", None, Some(RemoteNote::NoRemote)),
            found("ssh", None, Some(RemoteNote::Ssh)),
            found("http", None, Some(RemoteNote::NotHttps)),
        ],
    );
    seed(&api, "off", WorkspaceStatus::Stopped, &[], vec![]);
    for (workspace, body, status, part) in [
        (
            "web",
            json!({"kind": "clone", "url": "git@github.com:a/b.git"}),
            422,
            "SSH",
        ),
        (
            "web",
            json!({"kind": "clone", "url": "https://u:p@github.com/a/b"}),
            422,
            "***",
        ),
        (
            "web",
            json!({"kind": "clone", "url": "https://github.com/a/b", "directory": "../x"}),
            422,
            "must not start with",
        ),
        (
            "web",
            json!({"kind": "clone", "url": "https://github.com/a/b", "identity_id": 99}),
            422,
            "no identity 99",
        ),
        (
            "web",
            json!({"kind": "existing", "directory": "a/b"}),
            422,
            "directory",
        ),
        (
            "web",
            json!({"kind": "existing", "directory": "x", "identity_id": 99}),
            422,
            "no identity 99",
        ),
        ("web", json!({"kind": "teleport"}), 422, "unknown variant"),
        ("web", json!({"url": "https://github.com/a/b"}), 422, "kind"),
        (
            "web",
            json!({"kind": "clone", "url": "https://github.com/a/b", "repo_url": "x"}),
            422,
            "repo_url",
        ),
    ] {
        let reply = add(&api, workspace, body.clone()).await;
        assert_eq!(reply.status, status, "{workspace} {body}: {}", reply.body);
        assert!(
            reply.body.contains(part),
            "{workspace} {body}: {}",
            reply.body
        );
        assert!(
            !reply.body.contains("u:p@"),
            "a credential came back: {}",
            reply.body
        );
    }
    // Nothing was added by any of those.
    let ws = api.get("/api/workspaces/web").await.json();
    assert_eq!(
        ws["repositories"],
        json!([{"url": API, "directory": "api"}])
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_full_list_and_a_busy_workspace_refuse_an_add() {
    let api = start().await;
    let dirs: Vec<(String, String)> = (0..100)
        .map(|i| (format!("r{i}"), format!("https://github.com/acme/r{i}")))
        .collect();
    let refs: Vec<(&str, &str)> = dirs.iter().map(|(d, u)| (d.as_str(), u.as_str())).collect();
    seed(&api, "full", WorkspaceStatus::Running, &refs, vec![]);
    let reply = add(
        &api,
        "full",
        json!({"kind": "clone", "url": "https://github.com/acme/more"}),
    )
    .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert!(reply.body.contains("full"), "{}", reply.body);

    running(&api);
    api.workspaces.hold();
    assert_eq!(
        add(
            &api,
            "web",
            json!({"kind": "clone", "url": "https://github.com/a/one"})
        )
        .await
        .status,
        202
    );
    let reply = add(
        &api,
        "web",
        json!({"kind": "clone", "url": "https://github.com/a/two"}),
    )
    .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert!(reply.body.contains("busy"), "{}", reply.body);
    let reply = add(&api, "web", json!({"kind": "existing", "directory": "api"})).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    api.workspaces.release();
    api.workspaces.idle().await;
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_checkout_on_the_disk_is_managed_with_its_git_settings_and_changes_nothing_else() {
    let api = start().await;
    seed(
        &api,
        "web",
        WorkspaceStatus::Running,
        &[("api", API)],
        vec![
            found("api", Some(API), None),
            found("raw", Some("https://github.com/acme/raw"), None),
        ],
    );
    let mut stream = Stream::open(&api, "?workspace=web").await;
    let reply = add(
        &api,
        "web",
        json!({"kind": "existing", "directory": "raw", "pull": true, "push": false}),
    )
    .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let ws = reply.json();
    assert_eq!(ws["busy"], Value::Null);
    assert_eq!(
        ws["repositories"][1],
        json!({"url": "https://github.com/acme/raw", "directory": "raw"})
    );
    // The table row exists with the toggles asked for, before any clone would have run.
    let git = api.get("/api/workspaces/web/git").await.json();
    let rows = git["repos"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["repo"].to_string().contains("raw")
            && r["push"] == false
            && r["pull"] == true),
        "{git}"
    );
    stream
        .read_until(|b| b.contains("workspace_repositories_changed"))
        .await;
    let checkouts = api.get("/api/workspaces/web/checkouts").await.json();
    let raw = checkouts["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["directory"] == "raw")
        .unwrap();
    assert_eq!(raw["entry"]["directory"], "raw");
    api.running.shutdown().await;
}

#[tokio::test]
async fn cloning_an_entry_again_needs_the_directory_to_be_gone() {
    let api = start().await;
    seed(
        &api,
        "web",
        WorkspaceStatus::Running,
        &[("api", API), ("lost", "https://github.com/acme/lost")],
        vec![found("api", Some(API), None)],
    );
    let path = |dir: &str| format!("/api/workspaces/web/repositories/{dir}/clone");
    assert_eq!(post(&api, &path("unknown"), &json!({})).await.status, 404);
    assert_eq!(post(&api, &path("a%2Fb"), &json!({})).await.status, 404);
    let there = post(&api, &path("api"), &json!({})).await;
    assert_eq!(there.status, 409, "{}", there.body);
    assert!(there.body.contains("already exists"), "{}", there.body);
    let again = post(&api, &path("lost"), &json!({})).await;
    assert_eq!(again.status, 202, "{}", again.body);
    assert_eq!(again.json()["busy"], "cloning");
    api.workspaces.idle().await;
    let list = api.get("/api/workspaces/web").await.json();
    assert_eq!(
        list["repositories"].as_array().unwrap().len(),
        2,
        "the entry is not added twice"
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn removing_an_entry_forgets_it_and_nothing_else() {
    let api = start().await;
    running(&api);
    let mut stream = Stream::open(&api, "?workspace=web").await;
    let git_before = api.get("/api/workspaces/web/git").await.json();
    let gone = api
        .send("DELETE", "/api/workspaces/web/repositories/api", None)
        .await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    assert_eq!(gone.json()["repositories"], json!([]));
    stream
        .read_until(|b| b.contains("workspace_repositories_changed"))
        .await;
    // The checkout is still on the disk and now shows without an entry; the Git settings stay.
    let checkouts = api.get("/api/workspaces/web/checkouts").await.json();
    assert_eq!(checkouts["checkouts"][0]["directory"], "api");
    assert_eq!(checkouts["checkouts"][0]["entry"], Value::Null);
    assert_eq!(api.get("/api/workspaces/web/git").await.json(), git_before);
    for (path, status) in [
        ("/api/workspaces/web/repositories/api", 404),
        ("/api/workspaces/web/repositories/a%2Fb", 404),
        ("/api/workspaces/nope/repositories/api", 404),
    ] {
        assert_eq!(
            api.send("DELETE", path, None).await.status,
            status,
            "{path}"
        );
    }
    // Allowed while stopped.
    seed(
        &api,
        "off",
        WorkspaceStatus::Stopped,
        &[("api", API)],
        vec![],
    );
    assert_eq!(
        api.send("DELETE", "/api/workspaces/off/repositories/api", None)
            .await
            .status,
        200
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn removing_an_entry_is_refused_while_the_workspace_is_being_created() {
    let api = start().await;
    api.workspaces.hold();
    let created = post(
        &api,
        "/api/workspaces",
        &json!({"name": "new", "repositories": [{"url": API}]}),
    )
    .await;
    assert_eq!(created.status, 202, "{}", created.body);
    let reply = api
        .send("DELETE", "/api/workspaces/new/repositories/api", None)
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    api.workspaces.release();
    api.workspaces.idle().await;
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_contract_lists_the_new_routes_and_the_cloning_operation() {
    let text = puddle_api::openapi_json();
    let api = start().await;
    for part in [
        "/api/workspaces/{id}/checkouts",
        "/api/workspaces/{id}/repositories",
        "/api/workspaces/{id}/repositories/{directory}/clone",
        "/api/workspaces/{id}/repositories/{directory}",
        "workspace_repositories_changed",
        "\"cloning\"",
    ] {
        assert!(text.contains(part), "{part} missing from the contract");
    }
    api.running.shutdown().await;
}
