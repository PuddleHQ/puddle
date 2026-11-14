// SPDX-License-Identifier: AGPL-3.0-or-later
//! The managed tools install after a start: it begins only once the start is done, a failure is
//! the `tools:<id>` problem (never a failed start), and a host the install waited on that the
//! user allows runs it once more. The fake guest answers `puddle-agent mise-install` like the
//! agent; `puddle_agent::mise_install` has the guest side's own tests.

use puddle_types::SuffixAllows;
use serde_json::Value;

use super::*;

const NODE: &str = "node@22.11.0";
const NODE_FAILURE: &str = "mise ERROR HTTP 403 Forbidden for https://nodejs.org/dist/v22.11.0";

/// A rig with the layer on, whose guest misses `tools`.
fn rig_missing(tools: &[&str]) -> Rig {
    let rig = Rig::named_ids();
    rig.mise_on.store(true, Ordering::SeqCst);
    *rig.guest.tools_missing.lock().unwrap() = tools.iter().map(|t| (*t).to_owned()).collect();
    rig
}

/// Matches the `workspace_tools` event of `acme` in `phase`.
fn tools_in(phase: &'static str) -> impl Fn(&Value) -> bool {
    move |e| {
        e["type"] == "workspace_tools" && e["workspace"] == "acme" && e["state"]["phase"] == phase
    }
}

fn has_tools_problem(host: &Host<FakeRuntime>) -> bool {
    host.problems().list().iter().any(|p| p.key == "tools:acme")
}

async fn restart_acme(api: &Api, events: &mut support::Events) {
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_install_begins_after_the_start_is_done_and_a_second_start_installs_nothing() {
    let rig = rig_missing(&[NODE]);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let installing = events
        .until(tools_in("installing"), Duration::from_secs(20))
        .await;
    assert_eq!(installing["state"]["tools"], json!([NODE]));
    assert_eq!(installing["state"]["done"], 0);
    assert_eq!(installing["state"]["total"], 1);
    events
        .until(tools_in("ready"), Duration::from_secs(20))
        .await;
    let runs = rig.guest.tools_runs.lock().unwrap().clone();
    assert_eq!(
        runs.first().unwrap().0,
        ["mise-install", "/workspaces/acme", "--missing"]
    );
    assert!(
        runs.iter()
            .all(|(_, user)| user.as_deref() == Some("agent")),
        "as the session user: {runs:?}"
    );
    assert_eq!(rig.guest.tool_installs(), 1);
    assert!(!has_tools_problem(&host));
    assert!(
        rig.guest.java_trusts.lock().unwrap().is_empty(),
        "no JDK came"
    );

    restart_acme(&api, &mut events).await;
    events
        .until(tools_in("ready"), Duration::from_secs(20))
        .await;
    assert_eq!(
        rig.guest.tool_installs(),
        1,
        "nothing missing: only a report, no install"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_install_is_a_problem_the_start_still_ends_done_and_a_later_good_one_clears_it() {
    let rig = rig_missing(&[NODE]);
    *rig.guest.tools_failure.lock().unwrap() = Some(NODE_FAILURE);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "running");
    let failed = events
        .until(tools_in("failed"), Duration::from_secs(20))
        .await;
    assert_eq!(failed["state"]["tools"], json!([NODE]));
    assert_eq!(failed["state"]["waiting_on"], json!([]));
    let problems = problems_of(&api).await;
    let (_, workspace, title, detail) = problems.iter().find(|p| p.0 == "tools:acme").unwrap();
    assert_eq!(workspace.as_deref(), Some("acme"));
    assert!(title.contains("tools"), "{title}");
    assert!(detail.contains(NODE), "names the tool: {detail}");
    assert!(detail.contains(NODE_FAILURE), "mise's line: {detail}");
    assert!(
        detail.contains("run `mise install` in a terminal"),
        "{detail}"
    );
    assert!(
        !detail.contains("Network tab"),
        "nothing waited on: {detail}"
    );

    *rig.guest.tools_failure.lock().unwrap() = None;
    restart_acme(&api, &mut events).await;
    events
        .until(tools_in("ready"), Duration::from_secs(20))
        .await;
    eventually("the problem ends", APPLY, || !has_tools_problem(&host)).await;
    host.shutdown().await;
}

/// A guest whose install asks for `host` through the proxy, which no rule decides: a pending row.
fn install_asks_for(rig: &Rig, host: &Host<FakeRuntime>, asked: &'static str) {
    let store = host.store().clone();
    *rig.guest.at_install.lock().unwrap() = Some(Box::new(move || {
        let request = puddle_types::EgressRequest::new(
            name("acme"),
            puddle_types::Host::parse_normalised(asked).unwrap(),
            443,
        );
        store.decide(&request, SuffixAllows::Count).unwrap();
    }));
}

fn open_row_of(host: &Host<FakeRuntime>, asked: &str) -> i64 {
    host.store()
        .open_pending_every_user(&puddle_store::testing::all_users(), Some(&name("acme")))
        .unwrap()
        .into_iter()
        .find(|row| row.host.to_string() == asked)
        .unwrap()
        .id
        .0
}

#[tokio::test(flavor = "multi_thread")]
async fn allowing_a_host_the_install_waited_on_runs_it_once_more_and_a_denial_does_not() {
    let rig = rig_missing(&[NODE]);
    *rig.guest.tools_failure.lock().unwrap() = Some(NODE_FAILURE);
    let host = rig.start().await;
    install_asks_for(&rig, &host, "nodejs.org");
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let failed = events
        .until(tools_in("failed"), Duration::from_secs(20))
        .await;
    assert_eq!(failed["state"]["waiting_on"], json!(["nodejs.org"]));
    let problems = problems_of(&api).await;
    let (_, _, _, detail) = problems.iter().find(|p| p.0 == "tools:acme").unwrap();
    assert!(
        detail.contains("nodejs.org") && detail.contains("Network tab"),
        "{detail}"
    );
    assert_eq!(rig.guest.tool_installs(), 1);

    // A denial: the install stays as it ended.
    let denied = open_row_of(&host, "nodejs.org");
    let reply = api.post(&format!("/api/pending/{denied}/deny"), "{}").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    no_install_for(&rig, 1).await;

    // The next install asks again, this time for a host the user allows.
    install_asks_for(&rig, &host, "nodejs.org.mirror.test");
    restart_acme(&api, &mut events).await;
    let failed = events
        .until(tools_in("failed"), Duration::from_secs(20))
        .await;
    assert_eq!(
        failed["state"]["waiting_on"],
        json!(["nodejs.org.mirror.test"])
    );
    assert_eq!(rig.guest.tool_installs(), 2);
    *rig.guest.tools_failure.lock().unwrap() = None;
    let allowed = open_row_of(&host, "nodejs.org.mirror.test");
    let reply = api
        .post(&format!("/api/pending/{allowed}/approve"), "{}")
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    events
        .until(tools_in("ready"), Duration::from_secs(20))
        .await;
    assert_eq!(rig.guest.tool_installs(), 3, "one more install");
    eventually("the problem ends", APPLY, || !has_tools_problem(&host)).await;
    no_install_for(&rig, 3).await;
    host.shutdown().await;
}

/// Shows that no further install starts: the count of installs stays at `installs`.
async fn no_install_for(rig: &Rig, installs: usize) {
    // An absence window: nothing signals an install that never starts.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(rig.guest.tool_installs(), installs);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clone_into_the_running_workspace_installs_its_tools() {
    let rig = rig_missing(&[]);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    events
        .until(tools_in("ready"), Duration::from_secs(20))
        .await;
    assert_eq!(rig.guest.tool_installs(), 0, "nothing was missing");
    // The repository's mise.toml asks for node.
    *rig.guest.tools_missing.lock().unwrap() = vec![NODE.to_owned()];
    let reply = api
        .post(
            "/api/workspaces/acme/repositories",
            &json!({"kind": "clone", "url": "https://example.org/acme/tooling"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events
        .until(tools_in("ready"), Duration::from_secs(20))
        .await;
    assert_eq!(rig.guest.tool_installs(), 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_jdk_gets_puddles_cas() {
    let rig = rig_missing(&["java@temurin-21.0.5+11.0.LTS"]);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    events
        .until(tools_in("ready"), Duration::from_secs(20))
        .await;
    eventually("the trust step runs", APPLY, || {
        !rig.guest.java_trusts.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(
        *rig.guest.java_trusts.lock().unwrap(),
        [None],
        "as root: it writes /etc/puddle"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_the_layer_off_nothing_is_installed() {
    let rig = Rig::named_ids();
    *rig.guest.tools_missing.lock().unwrap() = vec![NODE.to_owned()];
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // An absence window: an install that never starts sends nothing.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(rig.guest.tools_runs.lock().unwrap().is_empty());
    assert!(
        !events.seen.iter().any(|e| e["type"] == "workspace_tools"),
        "{:?}",
        events.seen
    );
    host.shutdown().await;
}
