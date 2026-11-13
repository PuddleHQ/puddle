// SPDX-License-Identifier: GPL-3.0-or-later
//! The workspaces resource over real HTTP, against `FakeWorkspaces`.

use puddle_api::DisplayName;
use puddle_api::{
    Listing, Operation, RepoFindings, RepoUrl, Repository, SettingsRepo, Unsaved, WorkspaceRecord,
};
use puddle_types::{EntryId, WorkspaceId, WorkspaceStatus};
use serde_json::{Value, json};

use crate::common::{Api, Reply, START_MS, start, start_without_workspaces};
use crate::events::Stream;

const REPO: &str = "https://github.com/acme/api.git";

/// The list of repositories without its generated entry ids, after checking that each has a
/// well-formed one and that no two are alike.
fn without_entries(list: &Value) -> Value {
    let mut list = list.clone();
    let entries: Vec<EntryId> = list
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .map(|r| {
            let entry = r.as_object_mut().unwrap().remove("entry").unwrap();
            EntryId::new(entry.as_str().unwrap()).unwrap()
        })
        .collect();
    let distinct: std::collections::BTreeSet<_> = entries.iter().collect();
    assert_eq!(
        distinct.len(),
        entries.len(),
        "each entry has an id of its own"
    );
    list
}

async fn create(api: &Api, name: &str) -> Value {
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": name, "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    reply.json()
}

async fn post(api: &Api, path: &str) -> Reply {
    api.send("POST", path, Some(&json!({}))).await
}

/// Creates `name` and waits until it exists and is idle.
async fn created(api: &Api, name: &str) {
    create(api, name).await;
    api.workspaces.idle().await;
}

/// Creates and starts `name`.
async fn running(api: &Api, name: &str) {
    created(api, name).await;
    assert_eq!(
        post(api, &format!("/api/workspaces/{name}/start"))
            .await
            .status,
        202
    );
    api.workspaces.idle().await;
}

fn seed(api: &Api, name: &str, status: WorkspaceStatus, unsaved: Unsaved) {
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new(name).unwrap(),
        DisplayName::parse(name).unwrap(),
        vec![Repository::at_default_directory(
            EntryId::from_random([7; 5]),
            RepoUrl::parse(REPO).unwrap(),
        )],
    );
    record.status = status;
    api.own(record.id.as_str());
    api.workspaces.seed(record, unsaved);
}

fn dirty() -> Unsaved {
    Unsaved {
        repos: vec![RepoFindings {
            dir: "api".into(),
            uncommitted: Listing {
                items: vec![" M src/main.rs".into(), "?? notes.txt".into()],
                more: 3,
            },
            unpushed: Listing {
                items: vec!["abc1234 wip".into()],
                more: 0,
            },
            stashes: Listing::default(),
        }],
        other: Listing {
            items: vec!["scratch".into()],
            more: 0,
        },
        errors: vec![],
    }
}

#[tokio::test]
async fn an_empty_install_lists_nothing() {
    let api = start().await;
    let reply = api.get("/api/workspaces").await;
    assert_eq!(reply.status, 200);
    assert_eq!(reply.json(), json!({"workspaces": []}));
    api.running.shutdown().await;
}

#[tokio::test]
async fn create_is_accepted_busy_and_finishes_with_progress_events() {
    let api = start().await;
    let mut stream = Stream::open(&api, "?workspace=web").await;
    let ws = create(&api, "web").await;
    assert_eq!(
        ws,
        json!({
            "id": "web",
            "name": "web",
            "repositories": [{
                "entry": ws["repositories"][0]["entry"],
                "url": REPO,
                "directory": "api"
            }],
            "start_folder": null,
            "session_user": "agent",
            "image": puddle_api::DEFAULT_IMAGE,
            "memory_mib": 8192,
            "status": "created",
            "busy": "creating",
            "created_at": START_MS,
            "disk_size_mib": puddle_api::DEFAULT_DISK_MIB,
            "disk_used_mib": 0,
            "disk_warnings": [],
            "root_disk_size_mib": 4096,
            "root_disk_used_mib": 0,
            "direct_ssh": false,
            "settings_error": null,
            "first_connect_notice_due": false,
            // No identity exists in this store, so the answer to creating says so.
            "identities": [{
                "identity": null,
                "basis": "none",
                "warning": ws["identities"][0]["warning"]
            }],
            // Nothing allowed github.com yet, so the create did, with the first rule of the store.
            "allowed_hosts": [{"host": "github.com", "rule_id": 1}]
        })
    );
    assert!(ws["identities"][0]["warning"].is_string());
    api.workspaces.idle().await;
    stream.read_until(|b| b.contains("\"step\":\"done\"")).await;
    let steps: Vec<String> = stream
        .data()
        .iter()
        .filter(|e| e["type"] == "workspace_progress")
        .map(|e| e["step"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        steps,
        ["preparing_volume", "pulling_image", "cloning", "done"]
    );
    let cloning: Vec<_> = stream
        .data()
        .iter()
        .filter(|e| e["type"] == "workspace_progress")
        .map(|e| {
            (
                e["step"].clone(),
                e["detail"].clone(),
                e["repository"].clone(),
            )
        })
        .filter(|(_, detail, _)| *detail == REPO)
        .collect();
    assert_eq!(cloning, [(json!("cloning"), json!(REPO), json!("api"))]);
    let detail = api.get("/api/workspaces/web").await.json();
    assert_eq!(
        detail["identities"],
        json!([]),
        "only creating answers with them"
    );
    assert_eq!(detail["busy"], Value::Null);
    assert_eq!(detail["status"], "created");
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"][0],
        detail
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn create_takes_an_image_and_memory() {
    let api = start().await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({
                "name": "big", "repositories": [{"url": REPO}],
                "image": "ghcr.io/acme/dev:1", "memory_mib": 16384
            })),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["image"], "ghcr.io/acme/dev:1");
    assert_eq!(reply.json()["memory_mib"], 16384);
    api.workspaces.idle().await;
    api.running.shutdown().await;
}

async fn make_identity(api: &Api, label: &str, owners: &[&str]) -> i64 {
    let reply = api
        .send(
            "POST",
            "/api/identities",
            Some(&json!({
                "label": label,
                "author": {"name": label, "email": format!("{}@example.org", label.to_lowercase())},
                "credentials": [{
                    "host": "github.com",
                    "source": {"kind": "gh", "host": "github.com", "account": "me"},
                    "covers": {"owners": owners, "rest_of_host": false}
                }]
            })),
        )
        .await;
    assert_eq!(reply.status, 201, "{}", reply.body);
    reply.json()["id"].as_i64().unwrap()
}

#[tokio::test]
async fn create_with_no_repositories_is_an_ordinary_workspace() {
    let api = start().await;
    let reply = api
        .send("POST", "/api/workspaces", Some(&json!({"name": "empty"})))
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let ws = reply.json();
    assert_eq!(
        (&ws["repositories"], &ws["identities"], &ws["start_folder"]),
        (&json!([]), &json!([]), &Value::Null)
    );
    api.workspaces.idle().await;
    let git = api.get("/api/workspaces/empty/git").await.json();
    assert_eq!(
        (&git["identities"], &git["repos"]),
        (&json!([]), &json!([]))
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn create_with_three_repositories_sets_each_up_and_answers_their_identities_in_order() {
    let api = start().await;
    let work = make_identity(&api, "Work", &["acme"]).await;
    let mine = make_identity(&api, "Mine", &["me"]).await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({
                "name": "trio",
                "start_folder": "web/src",
                "repositories": [
                    {"url": "https://github.com/acme/web.git"},
                    {"url": "https://github.com/me/notes", "directory": "my-notes", "identity_id": work, "push": false},
                    {"url": "https://github.com/me/blog"}
                ]
            })),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let ws = reply.json();
    let list = without_entries(&ws["repositories"]);
    assert_eq!(
        list,
        json!([
            {"url": "https://github.com/acme/web.git", "directory": "web"},
            {"url": "https://github.com/me/notes", "directory": "my-notes"},
            {"url": "https://github.com/me/blog", "directory": "blog"}
        ])
    );
    assert_eq!(ws["start_folder"], "web/src");
    let identities: Vec<_> = ws["identities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["identity"].clone(),
                i["basis"].clone(),
                i["warning"].clone(),
            )
        })
        .collect();
    assert_eq!(
        identities,
        [
            (json!("Work"), json!("covers"), Value::Null),
            (json!("Work"), json!("chosen"), Value::Null),
            (json!("Mine"), json!("covers"), Value::Null),
        ]
    );
    api.workspaces.idle().await;
    let git = api.get("/api/workspaces/trio/git").await.json();
    let attached: Vec<_> = git["identities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_i64().unwrap())
        .collect();
    assert_eq!(attached, [work, mine]);
    let rows: Vec<_> = git["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["owner"].clone(),
                r["repo"].clone(),
                r["pull"].clone(),
                r["push"].clone(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (json!("acme"), json!("web"), json!(true), json!(true)),
            (json!("me"), json!("notes"), json!(true), json!(false)),
            (json!("me"), json!("blog"), json!(true), json!(true)),
        ]
    );
    assert_eq!(
        api.get("/api/workspaces/trio").await.json()["repositories"],
        ws["repositories"]
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn create_with_two_identities_that_cover_the_same_place_is_a_409_and_makes_nothing() {
    let api = start().await;
    let a = make_identity(&api, "A", &["acme"]).await;
    let b = make_identity(&api, "B", &["acme"]).await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({
                "name": "clash",
                "repositories": [
                    {"url": "https://github.com/acme/one", "identity_id": a},
                    {"url": "https://github.com/acme/two", "identity_id": b}
                ]
            })),
        )
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert_eq!(reply.error(), "identity_collision");
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("A and B"), "{message}");
    assert_eq!(api.get("/api/workspaces/clash").await.status, 404);
    // Nothing of the refused create is left: the same name made again has only its own settings.
    let again = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "clash", "repositories": [{"url": "https://github.com/acme/one", "identity_id": b}]})),
        )
        .await;
    assert_eq!(again.status, 202, "{}", again.body);
    api.workspaces.idle().await;
    let git = api.get("/api/workspaces/clash/git").await.json();
    assert_eq!(git["identities"].as_array().unwrap().len(), 1, "{git}");
    assert_eq!(git["identities"][0]["id"], b);
    assert_eq!(git["repos"].as_array().unwrap().len(), 1, "{git}");
    api.running.shutdown().await;
}

/// Create bodies the API refuses with 422, each with a part of the message it gives.
fn refused_bodies() -> [(Value, &'static str); 20] {
    let long = "w".repeat(61);
    [
        (
            json!({"name": "a", "repositories": [{"url": "git@github.com:acme/api.git"}]}),
            "SSH remotes are not supported yet",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "ssh://git@github.com/acme/api.git"}]}),
            "SSH remotes are not supported yet",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "http://github.com/a/b"}]}),
            "https://",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "/srv/repo"}]}),
            "https://",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "https://me:pw@github.com/a/b"}]}),
            "user name and password",
        ),
        (
            json!({"name": "a", "repositories": [{"url": "https://github.com/a b"}]}),
            "spaces",
        ),
        (
            json!({"name": "", "repositories": [{"url": REPO}]}),
            "must not be empty",
        ),
        (
            json!({"name": "  ", "repositories": [{"url": REPO}]}),
            "must not be empty",
        ),
        (
            json!({"name": long, "repositories": [{"url": REPO}]}),
            "longer than 60",
        ),
        (
            json!({"name": "two\nlines", "repositories": [{"url": REPO}]}),
            "line breaks",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO}], "memory_mib": 10}),
            "memory",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO}], "image": "no spaces allowed"}),
            "image",
        ),
        (
            json!({"name": "a", "repo_url": REPO}),
            "repo_url was replaced by repositories, a list of {url}",
        ),
        (
            json!({"name": "a", "repo_url": null, "repositories": []}),
            "repo_url was replaced",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO}, {"url": "https://gitlab.com/b/api"}]}),
            "repositories 1 (https://github.com/acme/api.git) and 2 (https://gitlab.com/b/api) both go into api; give one of them another directory, such as api-2",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO}, {"url": "https://github.com/ACME/api", "directory": "two"}]}),
            "are the same repository, github.com/acme/api",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO, "identity_id": 77}]}),
            "repository 1 (https://github.com/acme/api.git): there is no identity 77",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO}], "start_folder": "../etc"}),
            "start folder",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO, "branch": "main"}]}),
            "branch",
        ),
        (
            json!({"name": "a", "repositories": [{"url": REPO}], "extra": 1}),
            "extra",
        ),
    ]
}

#[tokio::test]
async fn create_refuses_what_cannot_work_and_says_why() {
    let api = start().await;
    let cases = refused_bodies();
    for (body, wants) in cases {
        let reply = api.send("POST", "/api/workspaces", Some(&body)).await;
        assert_eq!(reply.status, 422, "{body}: {}", reply.body);
        assert_eq!(reply.error(), "invalid");
        let message = reply.json()["message"].as_str().unwrap().to_owned();
        assert!(message.contains(wants), "{body}: {message}");
        assert!(
            !message.contains("pw@"),
            "credentials are never echoed: {message}"
        );
    }
    let many: Vec<Value> = (0..101)
        .map(|n| json!({"url": format!("https://github.com/acme/r{n}")}))
        .collect();
    let too_many = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "a", "repositories": many})),
        )
        .await;
    assert_eq!(too_many.status, 422);
    assert!(
        too_many.json()["message"]
            .as_str()
            .unwrap()
            .contains("at most 100 repositories"),
        "{}",
        too_many.body
    );
    let array = api.send("POST", "/api/workspaces", Some(&json!([]))).await;
    assert_eq!(array.status, 422);
    assert_eq!(
        api.get("/api/workspaces").await.json(),
        json!({"workspaces": []})
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn create_refuses_a_taken_name_and_a_failed_create_leaves_nothing() {
    let api = start().await;
    created(&api, "web").await;
    let again = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "web", "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(again.status, 409);
    assert_eq!(again.error(), "conflict");

    let mut stream = Stream::open(&api, "?workspace=bad").await;
    api.workspaces
        .fail_next(Operation::Creating, "clone failed: repository not found");
    create(&api, "bad").await;
    api.workspaces.idle().await;
    stream
        .read_until(|b| b.contains("\"step\":\"failed\""))
        .await;
    assert!(stream.buf.contains("repository not found"));
    assert_eq!(api.get("/api/workspaces/bad").await.status, 404);
    api.running.shutdown().await;
}

#[tokio::test]
async fn start_and_stop_walk_the_states_and_report_them() {
    let api = start().await;
    created(&api, "web").await;
    let mut stream = Stream::open(&api, "?workspace=web").await;

    api.workspaces.hold();
    let started = post(&api, "/api/workspaces/web/start").await;
    assert_eq!(started.status, 202, "{}", started.body);
    assert_eq!(started.json()["status"], "starting");
    assert_eq!(started.json()["busy"], "starting");
    // Busy: a second request is refused, whatever it is.
    for path in ["start", "stop", "reclaim", "reset-root-disk"] {
        let busy = post(&api, &format!("/api/workspaces/web/{path}")).await;
        assert_eq!(busy.status, 409, "{path}");
        assert!(busy.json()["message"].as_str().unwrap().contains("busy"));
    }
    let busy = api
        .send(
            "POST",
            "/api/workspaces/web/grow-root-disk",
            Some(&json!({"size_mib": 99999})),
        )
        .await;
    assert_eq!(busy.status, 409, "{}", busy.body);
    api.workspaces.release();
    api.workspaces.idle().await;
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["status"],
        "running"
    );
    assert_eq!(post(&api, "/api/workspaces/web/start").await.status, 409);

    let stopped = post(&api, "/api/workspaces/web/stop").await;
    assert_eq!(stopped.status, 202);
    assert_eq!(stopped.json()["status"], "draining");
    api.workspaces.idle().await;
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["status"],
        "stopped"
    );
    assert_eq!(post(&api, "/api/workspaces/web/stop").await.status, 409);

    stream
        .read_until(|b| b.matches("\"step\":\"done\"").count() >= 2)
        .await;
    let statuses: Vec<String> = stream
        .data()
        .iter()
        .filter(|e| e["type"] == "status_changed")
        .map(|e| e["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, ["starting", "running", "draining", "stopped"]);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_failed_start_ends_crashed_and_can_be_started_again() {
    let api = start().await;
    created(&api, "web").await;
    api.workspaces
        .fail_next(Operation::Starting, "the VM did not boot");
    post(&api, "/api/workspaces/web/start").await;
    api.workspaces.idle().await;
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["status"],
        "crashed"
    );
    assert_eq!(post(&api, "/api/workspaces/web/start").await.status, 202);
    api.workspaces.idle().await;
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["status"],
        "running"
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn reclaim_shrinks_what_the_disk_holds() {
    let api = start().await;
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new("web").unwrap(),
        DisplayName::parse("web").unwrap(),
        Vec::new(),
    );
    record.status = WorkspaceStatus::Stopped;
    record.disk_used_mib = Some(1000);
    api.own(record.id.as_str());
    api.workspaces.seed(record, Unsaved::default());
    let reply = post(&api, "/api/workspaces/web/reclaim").await;
    assert_eq!(reply.status, 202);
    assert_eq!(reply.json()["busy"], "reclaiming");
    api.workspaces.idle().await;
    let ws = api.get("/api/workspaces/web").await.json();
    assert_eq!(ws["disk_used_mib"], 500);
    assert_eq!(ws["busy"], Value::Null);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_root_disk_figures_and_warnings_follow_the_threshold_and_a_reset_empties_it() {
    let api = start().await;
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new("web").unwrap(),
        DisplayName::parse("web").unwrap(),
        Vec::new(),
    );
    record.status = WorkspaceStatus::Running;
    record.root_disk_size_mib = Some(4096);
    record.root_disk_used_mib = Some(4000);
    api.own(record.id.as_str());
    api.workspaces.seed(record, Unsaved::default());
    let ws = api.get("/api/workspaces/web").await.json();
    assert_eq!(ws["root_disk_size_mib"], 4096);
    assert_eq!(ws["root_disk_used_mib"], 4000);
    assert_eq!(ws["disk_warnings"], json!(["root"]));

    // The threshold is the global setting; the list and the single answer both follow it.
    let put = api
        .send(
            "PUT",
            "/api/settings",
            Some(&json!({"ui": {"disk_warning_percent": 99}})),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["disk_warnings"], json!([]));

    // A running workspace is not reset; a stopped one empties its root disk.
    let refused = post(&api, "/api/workspaces/web/reset-root-disk").await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    post(&api, "/api/workspaces/web/stop").await;
    api.workspaces.idle().await;
    let reply = post(&api, "/api/workspaces/web/reset-root-disk").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "resetting_root_disk");
    api.workspaces.idle().await;
    let ws = api.get("/api/workspaces/web").await.json();
    assert_eq!(ws["root_disk_used_mib"], 0);
    assert_eq!(ws["busy"], Value::Null);
    api.running.shutdown().await;
}

async fn grow(api: &Api, size: u64) -> Reply {
    api.send(
        "POST",
        "/api/workspaces/web/grow-root-disk",
        Some(&json!({"size_mib": size})),
    )
    .await
}

#[tokio::test]
async fn growing_the_root_disk_answers_202_and_a_size_that_is_not_bigger_is_422() {
    let api = start().await;
    let mut record = WorkspaceRecord::new(
        WorkspaceId::new("web").unwrap(),
        DisplayName::parse("web").unwrap(),
        Vec::new(),
    );
    record.status = WorkspaceStatus::Running;
    record.root_disk_size_mib = Some(4096);
    api.own(record.id.as_str());
    api.workspaces.seed(record, Unsaved::default());
    let reply = grow(&api, 30000).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "growing_root_disk");
    api.workspaces.idle().await;
    let ws = api.get("/api/workspaces/web").await.json();
    assert_eq!(ws["root_disk_size_mib"], 30000, "{ws}");
    assert_eq!(ws["busy"], Value::Null);
    for size in [30000, 100] {
        let refused = grow(&api, size).await;
        assert_eq!(refused.status, 422, "{}", refused.body);
        assert!(refused.body.contains("only grows"), "{}", refused.body);
    }
    let unknown = api
        .send(
            "POST",
            "/api/workspaces/web/grow-root-disk",
            Some(&json!({"size_mib": 40000, "extra": 1})),
        )
        .await;
    assert_eq!(unknown.status, 422, "{}", unknown.body);
    api.running.shutdown().await;
}

#[tokio::test]
async fn unknown_workspaces_are_404_on_every_route() {
    let api = start().await;
    for id in ["nope", "Not_An_Id"] {
        let base = format!("/api/workspaces/{id}");
        for (method, path, body) in [
            ("GET", base.clone(), None),
            ("POST", format!("{base}/start"), Some(json!({}))),
            ("POST", format!("{base}/stop"), Some(json!({}))),
            ("POST", format!("{base}/reclaim"), Some(json!({}))),
            ("POST", format!("{base}/reset-root-disk"), Some(json!({}))),
            (
                "POST",
                format!("{base}/grow-root-disk"),
                Some(json!({"size_mib": 20000})),
            ),
            ("GET", format!("{base}/delete-check"), None),
            ("DELETE", base.clone(), Some(json!({"confirm": true}))),
            (
                "POST",
                format!("{base}/attach"),
                Some(json!({"mode": "browser"})),
            ),
        ] {
            let reply = api.send(method, &path, body.as_ref()).await;
            assert_eq!(reply.status, 404, "{method} {path}: {}", reply.body);
            assert_eq!(reply.error(), "not_found");
        }
    }
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_workspace_with_no_volume_has_its_own_state_and_can_be_deleted() {
    let api = start().await;
    // Unsaved work the fake would list is ignored: with no volume there is nothing to look at.
    seed(&api, "web", WorkspaceStatus::VolumeMissing, dirty());
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["status"],
        "volume_missing"
    );
    let check = api.get("/api/workspaces/web/delete-check").await.json();
    assert_eq!(check["volume_missing"], true);
    assert_eq!(check["clean"], true);
    assert_eq!(check["repos"], json!([]));

    let seen = json!({"confirm": true, "fingerprint": check["fingerprint"]});
    let accepted = api.send("DELETE", "/api/workspaces/web", Some(&seen)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    api.workspaces.idle().await;
    assert_eq!(api.get("/api/workspaces/web").await.status, 404);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_delete_check_lists_what_would_be_lost() {
    let api = start().await;
    seed(&api, "web", WorkspaceStatus::Stopped, dirty());
    seed(&api, "tidy", WorkspaceStatus::Running, Unsaved::default());
    let check = api.get("/api/workspaces/web/delete-check").await;
    assert_eq!(check.status, 200);
    let c = check.json();
    assert_eq!(c["workspace"], "web");
    assert_eq!(c["clean"], false);
    assert_eq!(c["removes_sandbox"], "web");
    assert_eq!(c["errors"], json!([]));
    assert_eq!(c["volume_missing"], false);
    assert_eq!(c["other"], json!({"items": ["scratch"], "more": 0}));
    let repo = &c["repos"][0];
    assert_eq!(repo["dir"], "api");
    assert_eq!(repo["clean"], false);
    assert_eq!(repo["uncommitted"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(repo["uncommitted"]["more"], 3);
    assert_eq!(repo["unpushed"]["items"][0], "abc1234 wip");
    assert_eq!(repo["stashes"], json!({"items": [], "more": 0}));
    assert_eq!(c["fingerprint"].as_str().unwrap().len(), 64);
    // The same state gives the same fingerprint.
    assert_eq!(api.get("/api/workspaces/web/delete-check").await.json(), c);

    let tidy = api.get("/api/workspaces/tidy/delete-check").await.json();
    assert_eq!(tidy["clean"], true);
    assert_eq!(
        tidy["removes_sandbox"],
        Value::Null,
        "a running one is stopped first"
    );
    assert_ne!(tidy["fingerprint"], c["fingerprint"]);
    api.running.shutdown().await;
}

#[tokio::test]
async fn delete_needs_an_explicit_confirmation() {
    let api = start().await;
    seed(&api, "web", WorkspaceStatus::Stopped, Unsaved::default());
    for body in [json!({"confirm": false}), json!({})] {
        let reply = api.send("DELETE", "/api/workspaces/web", Some(&body)).await;
        assert_eq!(reply.status, 422, "{body}: {}", reply.body);
    }
    let no_body = api.send("DELETE", "/api/workspaces/web", None).await;
    assert!(
        no_body.status == 415 || no_body.status == 422,
        "{}",
        no_body.status
    );
    assert_eq!(api.get("/api/workspaces/web").await.status, 200);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_clean_workspace_is_deleted_without_a_fingerprint() {
    let api = start().await;
    seed(&api, "web", WorkspaceStatus::Stopped, Unsaved::default());
    let mut stream = Stream::open(&api, "?workspace=web").await;
    let reply = api
        .send(
            "DELETE",
            "/api/workspaces/web",
            Some(&json!({"confirm": true})),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "deleting");
    api.workspaces.idle().await;
    assert_eq!(api.get("/api/workspaces/web").await.status, 404);
    assert_eq!(
        api.get("/api/workspaces").await.json(),
        json!({"workspaces": []})
    );
    stream.read_until(|b| b.contains("\"step\":\"done\"")).await;
    assert!(stream.buf.contains("\"step\":\"removing\""));
    api.running.shutdown().await;
}

#[tokio::test]
async fn unsaved_work_is_never_deleted_unseen() {
    let api = start().await;
    seed(&api, "web", WorkspaceStatus::Stopped, dirty());
    let delete = |fingerprint: Value| {
        let api = &api;
        async move {
            api.send(
                "DELETE",
                "/api/workspaces/web",
                Some(&json!({"confirm": true, "fingerprint": fingerprint})),
            )
            .await
        }
    };
    // Without the report's fingerprint: refused, nothing deleted.
    let blind = api
        .send(
            "DELETE",
            "/api/workspaces/web",
            Some(&json!({"confirm": true})),
        )
        .await;
    assert_eq!(blind.status, 409);
    assert!(
        blind.json()["message"]
            .as_str()
            .unwrap()
            .contains("not saved")
    );
    // A fingerprint of something else.
    assert_eq!(delete(json!("0".repeat(64))).await.status, 409);
    assert_eq!(api.get("/api/workspaces/web").await.status, 200);

    // The report the user saw, then the workspace changes before they confirm.
    let seen = api.get("/api/workspaces/web/delete-check").await.json();
    let mut more = dirty();
    more.repos[0].stashes.items.push("stash@{0}: wip".into());
    assert!(
        api.workspaces
            .set_unsaved(&WorkspaceId::new("web").unwrap(), more)
    );
    let changed = delete(seen["fingerprint"].clone()).await;
    assert_eq!(changed.status, 409);
    assert!(
        changed.json()["message"]
            .as_str()
            .unwrap()
            .contains("changed")
    );
    assert_eq!(api.get("/api/workspaces/web").await.status, 200);

    // Confirming exactly the report the user saw goes through.
    let seen = api.get("/api/workspaces/web/delete-check").await.json();
    let ok = delete(seen["fingerprint"].clone()).await;
    assert_eq!(ok.status, 202, "{}", ok.body);
    api.workspaces.idle().await;
    assert_eq!(api.get("/api/workspaces/web").await.status, 404);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_running_or_busy_workspace_cannot_be_deleted() {
    let api = start().await;
    running(&api, "web").await;
    let reply = api
        .send(
            "DELETE",
            "/api/workspaces/web",
            Some(&json!({"confirm": true})),
        )
        .await;
    assert_eq!(reply.status, 409);
    assert!(
        reply.json()["message"]
            .as_str()
            .unwrap()
            .contains("stop it")
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn attach_needs_a_running_workspace() {
    let api = start().await;
    created(&api, "web").await;
    for mode in ["desktop", "browser"] {
        let reply = api
            .send(
                "POST",
                "/api/workspaces/web/attach",
                Some(&json!({"mode": mode})),
            )
            .await;
        assert_eq!(reply.status, 409, "{mode}");
        assert!(
            reply.json()["message"]
                .as_str()
                .unwrap()
                .contains("start it first")
        );
    }
    assert_eq!(api.launcher.opened().len(), 0);
    api.running.shutdown().await;
}

#[tokio::test]
async fn browser_attach_opens_the_window_through_the_launcher_and_answers_opened() {
    let api = start().await;
    running(&api, "web").await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces/web/attach",
            Some(&json!({"mode": "browser"})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json(), json!({"opened": true, "message": null}));
    assert_eq!(
        api.launcher.browsers_opened(),
        [WorkspaceId::new("web").unwrap()]
    );
    assert_eq!(api.launcher.opened().len(), 0);
    // A browser attach needs no SSH: it works while direct SSH is off.
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["direct_ssh"],
        false
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn desktop_attach_opens_the_editor_when_direct_ssh_is_on() {
    let api = start().await;
    running(&api, "web").await;
    allow_direct_ssh(&api, "web").await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces/web/attach",
            Some(&json!({"mode": "desktop"})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json(), json!({"opened": true, "message": null}));
    assert_eq!(api.launcher.opened(), [WorkspaceId::new("web").unwrap()]);
    api.running.shutdown().await;
}

async fn allow_direct_ssh(api: &Api, workspace: &str) {
    let reply = api
        .send(
            "PUT",
            &format!("/api/settings/workspaces/{workspace}"),
            Some(&json!({"overrides": {"direct_ssh": true}})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

#[tokio::test]
async fn desktop_attach_is_refused_while_direct_ssh_is_off() {
    let api = start().await;
    running(&api, "web").await;
    let reply = api
        .send(
            "POST",
            "/api/workspaces/web/attach",
            Some(&json!({"mode": "desktop"})),
        )
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert!(
        reply.json()["message"]
            .as_str()
            .unwrap()
            .contains("direct SSH is off"),
        "{}",
        reply.body
    );
    assert_eq!(api.launcher.opened().len(), 0, "nothing was launched");
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_workspace_is_trusted_by_its_own_switch_or_the_global_default() {
    let api = start().await;
    running(&api, "web").await;
    running(&api, "db").await;
    let direct = |name: &'static str| {
        let api = &api;
        async move { api.get(&format!("/api/workspaces/{name}")).await.json()["direct_ssh"].clone() }
    };
    assert_eq!(
        (direct("web").await, direct("db").await),
        (json!(false), json!(false))
    );

    allow_direct_ssh(&api, "web").await;
    assert_eq!(
        (direct("web").await, direct("db").await),
        (json!(true), json!(false))
    );

    let reply = api
        .send(
            "PUT",
            "/api/settings",
            Some(&json!({"workspace_defaults": {"direct_ssh": true}})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        (direct("web").await, direct("db").await),
        (json!(true), json!(true))
    );

    // An override of off beats the global default.
    let reply = api
        .send(
            "PUT",
            "/api/settings/workspaces/db",
            Some(&json!({"overrides": {"direct_ssh": false}})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(direct("db").await, json!(false));
    let list = api.get("/api/workspaces").await.json();
    let flags: Vec<_> = list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["direct_ssh"].clone())
        .collect();
    assert!(
        flags.contains(&json!(true)) && flags.contains(&json!(false)),
        "{flags:?}"
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn changing_settings_tells_the_workspaces_service() {
    let api = start().await;
    api.own("web");
    assert_eq!(api.workspaces.settings_changes(), 0);
    allow_direct_ssh(&api, "web").await;
    assert_eq!(api.workspaces.settings_changes(), 1);
    let reply = api
        .send(
            "PUT",
            "/api/settings",
            Some(&json!({"workspace_defaults": {"direct_ssh": true}})),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(api.workspaces.settings_changes(), 2);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_desktop_that_cannot_open_says_why() {
    let api = start().await;
    running(&api, "web").await;
    allow_direct_ssh(&api, "web").await;
    api.launcher.fail_next("VS Code is not installed");
    let reply = api
        .send(
            "POST",
            "/api/workspaces/web/attach",
            Some(&json!({"mode": "desktop"})),
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.json(),
        json!({"opened": false, "message": "VS Code is not installed"})
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_browser_window_that_cannot_open_says_why() {
    let api = start().await;
    running(&api, "web").await;
    api.launcher.fail_next("the editor gave no session");
    let reply = api
        .send(
            "POST",
            "/api/workspaces/web/attach",
            Some(&json!({"mode": "browser"})),
        )
        .await;
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.json(),
        json!({"opened": false, "message": "the editor gave no session"})
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn attach_refuses_unknown_modes_and_fields() {
    let api = start().await;
    running(&api, "web").await;
    for body in [
        json!({"mode": "emacs"}),
        json!({}),
        json!({"mode": "desktop", "x": 1}),
    ] {
        let reply = api
            .send("POST", "/api/workspaces/web/attach", Some(&body))
            .await;
        assert_eq!(reply.status, 422, "{body}");
    }
    api.running.shutdown().await;
}

async fn rename(api: &Api, id: &str, body: &Value) -> Reply {
    api.send("PATCH", &format!("/api/workspaces/{id}"), Some(body))
        .await
}

async fn lookup(api: &Api, q: &str) -> Reply {
    let q: String = q
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    api.get(&format!("/api/workspaces/lookup?q={q}")).await
}

#[tokio::test]
async fn a_running_workspace_is_renamed_without_touching_its_id_or_state() {
    let api = start().await;
    running(&api, "web").await;
    let reply = rename(&api, "web", &json!({"name": "  Web shop "})).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let ws = reply.json();
    assert_eq!(
        (&ws["id"], &ws["name"], &ws["status"], &ws["busy"]),
        (
            &json!("web"),
            &json!("Web shop"),
            &json!("running"),
            &Value::Null
        )
    );
    assert!(ws.get("display_name").is_none(), "{ws}");
    // Every read shows it, and the id in paths is still the old one.
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["name"],
        "Web shop"
    );
    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["name"], "Web shop");
    // Naming it the same again, in other case, is not a clash with itself.
    assert_eq!(
        rename(&api, "web", &json!({"name": "WEB SHOP"}))
            .await
            .status,
        200
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_rename_that_clashes_with_another_workspace_is_a_409_ignoring_case() {
    let api = start().await;
    created(&api, "web").await;
    created(&api, "docs").await;
    assert_eq!(
        rename(&api, "web", &json!({"name": "Handbook"}))
            .await
            .status,
        200
    );
    for taken in ["handbook", "HANDBOOK "] {
        let reply = rename(&api, "docs", &json!({"name": taken})).await;
        assert_eq!(reply.status, 409, "{taken}: {}", reply.body);
        assert_eq!(reply.error(), "conflict");
        assert!(
            reply.json()["message"]
                .as_str()
                .unwrap()
                .contains("already called Handbook")
        );
    }
    // A workspace renamed away from "web" frees that name for another to take.
    assert_eq!(
        rename(&api, "docs", &json!({"name": "web"})).await.status,
        200
    );
    // "Handbook" is not free for a new workspace while a workspace has it.
    let create = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "handbook", "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(create.status, 409, "{}", create.body);
    let taken_by_rename = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "web", "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(taken_by_rename.status, 409, "{}", taken_by_rename.body);
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_rename_is_refused_while_the_workspace_is_created_or_deleted() {
    let api = start().await;
    api.workspaces.hold();
    create(&api, "web").await;
    let reply = rename(&api, "web", &json!({"name": "Web"})).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    api.workspaces.release();
    api.workspaces.idle().await;
    assert_eq!(
        rename(&api, "web", &json!({"name": "Web"})).await.status,
        200
    );
    // Starting and stopping are not in the way: only the name changes.
    api.workspaces.hold();
    assert_eq!(post(&api, "/api/workspaces/web/start").await.status, 202);
    let during = rename(&api, "web", &json!({"name": "Web 2"})).await;
    assert_eq!(during.status, 200, "{}", during.body);
    assert_eq!(during.json()["busy"], "starting");
    api.workspaces.release();
    api.workspaces.idle().await;
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_rename_refuses_bad_names_and_bodies_and_unknown_workspaces() {
    let api = start().await;
    created(&api, "web").await;
    let long = "a".repeat(61);
    for body in [
        json!({"name": ""}),
        json!({"name": "   "}),
        json!({"name": long}),
        json!({"name": "two\nlines"}),
        json!({"name": null}),
        // The retired field, a missing field and an extra one are refused, never a reset.
        json!({"display_name": "x"}),
        json!({}),
        json!({"name": "x", "display_name": "y"}),
        json!({"name": 5}),
    ] {
        let reply = rename(&api, "web", &body).await;
        assert_eq!(reply.status, 422, "{body}: {}", reply.body);
    }
    assert_eq!(api.get("/api/workspaces/web").await.json()["name"], "web");
    assert_eq!(
        rename(&api, "ghost", &json!({"name": "x"})).await.status,
        404
    );
    // A name is never looked up: it is no id.
    assert_eq!(
        rename(&api, "Not_An_Id", &json!({"name": "x"}))
            .await
            .status,
        404
    );
    // Sixty characters, counted as characters, are accepted.
    let sixty = "\u{e9}".repeat(60);
    assert_eq!(
        rename(&api, "web", &json!({"name": sixty})).await.status,
        200
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn a_name_of_spaces_capitals_and_other_scripts_creates_a_workspace_known_by_its_id() {
    let api = start().await;
    let ws = create(&api, "  Shop API \u{fc}  ").await;
    assert_eq!(ws["name"], "Shop API \u{fc}");
    let id = ws["id"].as_str().unwrap().to_owned();
    assert_eq!(id, "shop-api");
    api.workspaces.idle().await;
    // Every call takes the id; the name is no path.
    assert_eq!(api.get("/api/workspaces/shop-api").await.status, 200);
    assert_eq!(
        api.get("/api/workspaces/Shop%20API%20%C3%BC").await.status,
        404
    );
    // The same name in other case is a clash.
    let reply = api
        .send(
            "POST",
            "/api/workspaces",
            Some(&json!({"name": "shop api \u{dc}", "repositories": [{"url": REPO}]})),
        )
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert_eq!(reply.error(), "conflict");
    // Another name that makes the same label is its own workspace, with an id of its own.
    let second = create(&api, "shop-api").await;
    assert_eq!(
        (second["name"].as_str(), second["id"].as_str()),
        (Some("shop-api"), Some("shop-api-2"))
    );
    api.workspaces.idle().await;
    // A name no DNS label could hold still creates, with an id of its own.
    let other = create(&api, "\u{65e5}\u{672c}\u{8a9e}").await;
    assert_eq!(other["name"], "\u{65e5}\u{672c}\u{8a9e}");
    assert!(
        other["id"].as_str().unwrap().starts_with("workspace-"),
        "{other}"
    );
    // The events of one workspace are asked for by its id.
    let _stream = Stream::open(&api, "?workspace=shop-api").await;
    api.workspaces.idle().await;
    api.running.shutdown().await;
}

#[tokio::test]
async fn create_takes_text_the_old_rules_refused_and_checks_only_the_text_rules() {
    let api = start().await;
    // Reserved shell names and a leading `m--` were names that could not be; as text they are fine.
    for name in ["tauri", "m--a", "A_b", "a.b"] {
        let ws = create(&api, name).await;
        assert_eq!(ws["name"], name);
        api.workspaces.idle().await;
    }
    // "A_b" and "a.b" make the same label: the second gets a number.
    let ids = api.get("/api/workspaces").await.json();
    let ids: Vec<&str> = ids["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"a-b") && ids.contains(&"a-b-2"), "{ids:?}");
    let ids: Vec<String> = api.get("/api/workspaces").await.json()["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        !ids.iter()
            .any(|i| ["tauri", "ipc", "asset"].contains(&i.as_str())),
        "{ids:?}"
    );
    assert!(ids.iter().all(|i| !i.starts_with("m--")), "{ids:?}");
    api.running.shutdown().await;
}

#[tokio::test]
async fn lookup_answers_one_none_or_two() {
    let api = start().await;
    created(&api, "Shop API \u{fc}").await;
    created(&api, "docs").await;
    // One: an exact id, or a name ignoring case and the spaces at the ends.
    for q in ["shop-api", "shop api \u{dc}", " SHOP API \u{fc} "] {
        let reply = lookup(&api, q).await;
        assert_eq!(reply.status, 200, "{q}: {}", reply.body);
        assert_eq!(
            reply.json(),
            json!({"id": "shop-api", "name": "Shop API \u{fc}"})
        );
    }
    assert_eq!(
        lookup(&api, "docs").await.json(),
        json!({"id": "docs", "name": "docs"})
    );
    // None.
    let none = lookup(&api, "nothing").await;
    assert_eq!(none.status, 404, "{}", none.body);
    assert_eq!(none.error(), "not_found");
    // Empty text is a mistake, not "nothing".
    assert_eq!(lookup(&api, " ").await.status, 422);
    let extra = api.get("/api/workspaces/lookup?q=a&x=1").await;
    assert_eq!(extra.status, 400, "{}", extra.body);
    // Two: a legacy id that is also another workspace's name.
    seed(&api, "legacy", WorkspaceStatus::Stopped, Unsaved::default());
    let mut other = WorkspaceRecord::new(
        WorkspaceId::new("k7m2x9qa").unwrap(),
        DisplayName::parse("Another").unwrap(),
        Vec::new(),
    );
    other.status = WorkspaceStatus::Stopped;
    api.own(other.id.as_str());
    api.workspaces.seed(other, Unsaved::default());
    // The legacy workspace is renamed away from its id, then the other takes the id as its name.
    let first = rename(&api, "legacy", &json!({"name": "Legacy shop"})).await;
    assert_eq!(first.status, 200, "{}", first.body);
    let second = rename(&api, "k7m2x9qa", &json!({"name": "legacy"})).await;
    assert_eq!(second.status, 200, "{}", second.body);
    let both = lookup(&api, "legacy").await;
    assert_eq!(both.status, 409, "{}", both.body);
    let body = both.json();
    assert_eq!(body["error"], "conflict");
    assert_eq!(
        body["matches"],
        json!([
            {"id": "k7m2x9qa", "name": "legacy"},
            {"id": "legacy", "name": "Legacy shop"}
        ])
    );
    assert!(
        body["message"].as_str().unwrap().contains("use the id"),
        "{body}"
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn without_a_workspaces_service_every_route_says_unavailable() {
    let api = start_without_workspaces().await;
    api.own("a");
    for (method, path, body) in [
        ("GET", "/api/workspaces", None),
        (
            "POST",
            "/api/workspaces",
            Some(json!({"name": "a", "repositories": [{"url": REPO}]})),
        ),
        ("GET", "/api/workspaces/a", None),
        ("GET", "/api/workspaces/lookup?q=a", None),
        ("PATCH", "/api/workspaces/a", Some(json!({"name": "A"}))),
        ("POST", "/api/workspaces/a/start", Some(json!({}))),
        ("POST", "/api/workspaces/a/stop", Some(json!({}))),
        ("POST", "/api/workspaces/a/reclaim", Some(json!({}))),
        ("POST", "/api/workspaces/a/reset-root-disk", Some(json!({}))),
        (
            "POST",
            "/api/workspaces/a/grow-root-disk",
            Some(json!({"size_mib": 20000})),
        ),
        ("GET", "/api/workspaces/a/delete-check", None),
        (
            "DELETE",
            "/api/workspaces/a",
            Some(json!({"confirm": true})),
        ),
        (
            "POST",
            "/api/workspaces/a/attach",
            Some(json!({"mode": "browser"})),
        ),
    ] {
        let reply = api.send(method, path, body.as_ref()).await;
        assert_eq!(reply.status, 503, "{method} {path}: {}", reply.body);
        assert_eq!(reply.error(), "unavailable");
    }
    api.running.shutdown().await;
}

#[tokio::test]
async fn unreadable_global_settings_are_reported_on_every_workspace_not_shown_as_off() {
    let api = start().await;
    running(&api, "web").await;
    api.settings.save_global(json!("not an object")).unwrap();
    for body in [
        api.get("/api/workspaces/web").await.json(),
        api.get("/api/workspaces").await.json()["workspaces"][0].clone(),
    ] {
        assert_eq!(body["direct_ssh"], false);
        let reason = body["settings_error"].as_str().unwrap();
        assert!(reason.contains("must be an object"), "{reason}");
        assert!(reason.contains("direct SSH counts as off"), "{reason}");
    }
    api.running.shutdown().await;
}

#[tokio::test]
async fn unreadable_workspace_settings_are_reported_on_that_workspace_only() {
    let api = start().await;
    running(&api, "web").await;
    running(&api, "db").await;
    api.settings
        .save_workspace(&WorkspaceId::new("web").unwrap(), json!([1]))
        .unwrap();
    assert!(api.get("/api/workspaces/web").await.json()["settings_error"].is_string());
    assert_eq!(
        api.get("/api/workspaces/db").await.json()["settings_error"],
        Value::Null
    );
    api.running.shutdown().await;
}

#[tokio::test]
async fn desktop_attach_names_the_unreadable_settings_instead_of_saying_direct_ssh_is_off() {
    let api = start().await;
    running(&api, "web").await;
    api.settings.save_global(json!(7)).unwrap();
    let reply = api
        .send(
            "POST",
            "/api/workspaces/web/attach",
            Some(&json!({"mode": "desktop"})),
        )
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("settings cannot be read"), "{message}");
    assert!(!message.contains("direct SSH is off for this workspace"));
    assert_eq!(api.launcher.opened().len(), 0);
    api.running.shutdown().await;
}

#[tokio::test]
async fn the_session_user_is_set_and_read_back_and_only_agent_or_root_is_taken() {
    let api = start().await;
    running(&api, "web").await;
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["session_user"],
        "agent"
    );
    let put = |body: Value| {
        let api = &api;
        async move {
            api.send("PUT", "/api/workspaces/web/session-user", Some(&body))
                .await
        }
    };
    let set = put(json!({"session_user": "root"})).await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(set.json()["session_user"], "root");
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["session_user"],
        "root"
    );
    for body in [
        json!({"session_user": "vscode"}),
        json!({}),
        json!({"session_user": "agent", "x": 1}),
    ] {
        assert_eq!(put(body.clone()).await.status, 422, "{body}");
    }
    let missing = api
        .send(
            "PUT",
            "/api/workspaces/nope/session-user",
            Some(&json!({"session_user": "agent"})),
        )
        .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    api.running.shutdown().await;
}

async fn put_start_folder(api: &Api, id: &str, body: &Value) -> Reply {
    api.send(
        "PUT",
        &format!("/api/workspaces/{id}/start-folder"),
        Some(body),
    )
    .await
}

#[tokio::test]
async fn the_start_folder_is_set_read_back_and_reset_and_a_bad_path_says_which_rule() {
    let api = start().await;
    running(&api, "web").await;
    let set = put_start_folder(&api, "web", &json!({"path": "app/src"})).await;
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(
        set.json(),
        json!({"start_folder": "app/src", "exists": true})
    );
    assert_eq!(
        api.get("/api/workspaces/web").await.json()["start_folder"],
        "app/src"
    );
    let reset = put_start_folder(&api, "web", &json!({"path": null})).await;
    assert_eq!(reset.json(), json!({"start_folder": null, "exists": null}));

    for (path, rule) in [
        ("", "empty"),
        ("/abs", "leading /"),
        ("a/../b", ".."),
        ("a//b", "empty"),
        (".puddle/x", ".puddle"),
        ("a\\b", "\\"),
    ] {
        let refused = put_start_folder(&api, "web", &json!({"path": path})).await;
        assert_eq!(refused.status, 422, "{path}: {}", refused.body);
        assert!(refused.body.contains(rule), "{path}: {}", refused.body);
    }
    // The body must say the path, and nothing else.
    for body in [json!({}), json!({"path": "a", "x": 1})] {
        assert_eq!(
            put_start_folder(&api, "web", &body).await.status,
            422,
            "{body}"
        );
    }
    assert_eq!(
        put_start_folder(&api, "nope", &json!({"path": null}))
            .await
            .status,
        404
    );
    api.running.shutdown().await;
}
