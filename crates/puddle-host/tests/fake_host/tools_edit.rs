// SPDX-License-Identifier: AGPL-3.0-or-later
//! The Tools card's calls against the whole host: the fake guest answers `puddle-agent mise-tools`
//! like the agent, over a stand-in root `mise.toml`. `puddle_agent::mise_tools` has the guest
//! side's own tests; here are the host's: the hash check, what a stopped workspace answers, what
//! happens after an edit, and how a failing mise or a hostile answer reads.

use serde_json::Value;

use super::*;

fn rig_on() -> Rig {
    let rig = Rig::named_ids();
    rig.mise_on.store(true, Ordering::SeqCst);
    rig
}

async fn running(rig: &Rig) -> (Host<FakeRuntime>, Api) {
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    (host, api)
}

fn change(version: &str, hash: &str) -> String {
    json!({"version": version, "sha256": hash}).to_string()
}

async fn listing(api: &Api) -> Value {
    let reply = api.get("/api/workspaces/acme/tools").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()
}

fn runs(rig: &Rig) -> Vec<Vec<String>> {
    rig.guest
        .mise_tools_runs
        .lock()
        .unwrap()
        .iter()
        .map(|(words, _)| words.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_edit_goes_through_the_agent_as_the_session_user_and_installs_behind_the_answer() {
    let rig = rig_on();
    let (host, api) = running(&rig).await;
    let first = listing(&api).await;
    assert_eq!(first["sha256"], "absent");
    assert_eq!(first["files"], json!([]));

    let added = api
        .put("/api/workspaces/acme/tools/node", &change("22", "absent"))
        .await;
    assert_eq!(added.status, 200, "{}", added.body);
    let added = added.json();
    assert_eq!(
        added["files"][0]["tools"],
        json!([{"name": "node", "requested": "22", "installed": false}])
    );
    let hash = added["sha256"].as_str().unwrap().to_owned();
    assert_eq!(hash.len(), 64);
    assert!(
        rig.guest
            .mise_tools_runs
            .lock()
            .unwrap()
            .iter()
            .all(|(_, user)| user.as_deref() == Some("agent")),
        "as the session user"
    );
    assert_eq!(
        runs(&rig)
            .iter()
            .filter(|w| w[0] == "set")
            .collect::<Vec<_>>(),
        [&vec![
            "set".to_owned(),
            "node".to_owned(),
            "22".to_owned(),
            "absent".to_owned()
        ]]
    );
    eventually("the install runs behind the edit", APPLY, || {
        rig.guest
            .tools_runs
            .lock()
            .unwrap()
            .iter()
            .any(|(args, _)| args.get(2).map(String::as_str) == Some("--missing"))
    })
    .await;

    let removed = api
        .delete(
            &format!("/api/workspaces/acme/tools/node?sha256={hash}"),
            None,
        )
        .await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    assert_eq!(removed.json()["files"], json!([]));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_that_changed_since_the_listing_answers_409_and_the_file_stays() {
    let rig = rig_on();
    let (host, api) = running(&rig).await;
    api.put("/api/workspaces/acme/tools/node", &change("22", "absent"))
        .await;
    let stale = api
        .put("/api/workspaces/acme/tools/go", &change("1.23", "absent"))
        .await;
    assert_eq!(stale.status, 409, "{}", stale.body);
    assert!(
        stale.body.contains("changed since it was listed"),
        "{}",
        stale.body
    );
    let stale = api
        .delete(
            &format!("/api/workspaces/acme/tools/node?sha256={}", "b".repeat(64)),
            None,
        )
        .await;
    assert_eq!(stale.status, 409, "{}", stale.body);
    let files = listing(&api).await["files"].clone();
    assert_eq!(files[0]["tools"].as_array().unwrap().len(), 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspace_answers_the_last_listing_and_refuses_the_rest() {
    let rig = rig_on();
    let (host, api) = running(&rig).await;
    let mut events = api.events().await;
    api.put("/api/workspaces/acme/tools/node", &change("22", "absent"))
        .await;
    let live = listing(&api).await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let before = runs(&rig).len();
    let stopped = listing(&api).await;
    assert_eq!(stopped["running"], false);
    assert_eq!(stopped["files"], live["files"]);
    assert!(stopped["problem"].as_str().unwrap().contains("not running"));
    for reply in [
        api.put("/api/workspaces/acme/tools/go", &change("1", "absent"))
            .await,
        api.get("/api/workspaces/acme/tools/registry").await,
        api.get("/api/workspaces/acme/tools/node/versions").await,
        api.post("/api/workspaces/acme/tools/install", "").await,
    ] {
        assert_eq!(reply.status, 409, "{}", reply.body);
    }
    assert_eq!(runs(&rig).len(), before, "the guest was not asked");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_layer_off_nothing_asks_the_guest_and_edits_answer_409() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let body = listing(&api).await;
    assert_eq!(body["problem"], "tools are off for this workspace");
    assert_eq!(body["running"], true);
    let reply = api
        .put("/api/workspaces/acme/tools/node", &change("22", "absent"))
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert!(runs(&rig).is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_registry_is_asked_once_and_a_failed_ask_is_not_kept() {
    let rig = rig_on();
    let (host, api) = running(&rig).await;
    *rig.guest.mise_tools_failure.lock().unwrap() = Some("mise ERROR registry broke");
    let failed = api.get("/api/workspaces/acme/tools/registry").await;
    assert_eq!(failed.status, 200, "{}", failed.body);
    assert_eq!(
        failed.json(),
        json!({"tools": [], "problem": "mise ERROR registry broke"})
    );
    *rig.guest.mise_tools_failure.lock().unwrap() = None;
    for _ in 0..2 {
        let reply = api.get("/api/workspaces/acme/tools/registry").await;
        assert_eq!(
            reply.json(),
            json!({"tools": [{"name": "node", "description": "JavaScript runtime"}], "problem": null})
        );
    }
    let asked = runs(&rig).iter().filter(|w| w[0] == "registry").count();
    assert_eq!(asked, 2, "one failed ask and one that was kept");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn versions_come_newest_first_and_a_mise_that_cannot_list_them_gives_its_reason() {
    let rig = rig_on();
    let (host, api) = running(&rig).await;
    let ok = api.get("/api/workspaces/acme/tools/node/versions").await;
    assert_eq!(
        ok.json(),
        json!({"versions": ["22.1.0", "22.0.0"], "reason": null})
    );
    *rig.guest.mise_tools_failure.lock().unwrap() =
        Some("mise ERROR no route to mise-versions.jdx.dev");
    let denied = api.get("/api/workspaces/acme/tools/node/versions").await;
    assert_eq!(denied.status, 200, "{}", denied.body);
    assert_eq!(
        denied.json(),
        json!({"versions": [], "reason": "mise ERROR no route to mise-versions.jdx.dev"})
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_mise_is_a_503_for_an_edit_and_a_problem_for_a_listing() {
    let rig = rig_on();
    let (host, api) = running(&rig).await;
    *rig.guest.mise_tools_failure.lock().unwrap() = Some("mise ERROR config file is read-only");
    let edit = api
        .put("/api/workspaces/acme/tools/node", &change("22", "absent"))
        .await;
    assert_eq!(edit.status, 503, "{}", edit.body);
    assert!(
        edit.body.contains("mise ERROR config file is read-only"),
        "{}",
        edit.body
    );
    let body = listing(&api).await;
    assert_eq!(body["problem"], "mise ERROR config file is read-only");
    assert_eq!(body["sha256"], Value::Null);
    *rig.guest.mise_tools_failure.lock().unwrap() = None;
    rig.guest.mise_tools_garbled.store(true, Ordering::SeqCst);
    let body = listing(&api).await;
    assert_eq!(body["problem"], "the guest's answer could not be read");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_install_retry_runs_the_install_again() {
    let rig = rig_on();
    let (host, api) = running(&rig).await;
    let before = rig.guest.tools_runs.lock().unwrap().len();
    let reply = api.post("/api/workspaces/acme/tools/install", "").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    eventually("a report of what is missing", APPLY, || {
        rig.guest.tools_runs.lock().unwrap().len() > before
    })
    .await;
    host.shutdown().await;
}
