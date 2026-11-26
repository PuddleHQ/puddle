// SPDX-License-Identifier: AGPL-3.0-or-later
//! A busy day of one user on a real host and runtime (tiers K and W): eight workspaces made one
//! after the other, then four of them deleted and their names made again in two concurrent pairs,
//! all eight stopped at once and started at once, and a quit and a new launch of Puddle. Every
//! step is read from the API the app talks to, from the runtime's own volumes and from the
//! workspace book on disk, never from the host's own bookkeeping alone.
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::print_stderr,
    reason = "VM test: a failed step fails the test; helpers outside #[test] fns panic; the timings go to stderr for the log"
)]

use puddle_host_test_support::support;
use puddle_host_test_support::vm_support;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use puddle_compute::Runtime;
use puddle_test_kit::eventually_async;
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use support::{Api, Events, ended};
use vm_support::{all_ended, guest, start_vm_host};

/// How long one workspace operation, or one batch of concurrent ones, may sit without any end.
const OP: Duration = Duration::from_secs(600);

/// How long one create may take before it counts as hung, on a loaded shared host.
const CREATE_HANG: Duration = Duration::from_secs(300);

/// How many times the first four creates' median time the last four's may take.
const CREATE_SLOWDOWN: u32 = 3;

/// How long eight workspaces may take to start together (each boots its own 512 MiB machine).
const START_ALL_BOUND: Duration = Duration::from_secs(300);

/// How long eight workspaces may take to stop together.
const STOP_ALL_BOUND: Duration = Duration::from_secs(180);

/// A create request for an alpine workspace with no repository and 512 MiB, answered at once.
async fn post_create(api: &Api, name: &str) -> support::Reply {
    let body = json!({"name": name, "repositories": [], "image": "alpine:3", "memory_mib": 512});
    api.post("/api/workspaces", &body.to_string()).await
}

/// Creates `name`, waits until it is done, and returns its id and how long that took.
async fn create_one(api: &Api, events: &mut Events, name: &str) -> (String, Duration) {
    let started = Instant::now();
    let reply = post_create(api, name).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    let end = events.until(ended(&id), OP).await;
    assert_eq!(end["step"], "done", "{end}");
    (id, started.elapsed())
}

/// Creates every name of `names` at once (all requests are in flight before any answers) and
/// returns the ids, in order, once every create is done.
async fn create_together(api: &Api, events: &mut Events, names: &[String]) -> Vec<String> {
    let replies =
        futures_util::future::join_all(names.iter().map(|name| post_create(api, name))).await;
    let mut ids = Vec::new();
    for reply in &replies {
        assert_eq!(reply.status, 202, "{}", reply.body);
        ids.push(reply.json()["id"].as_str().unwrap().to_owned());
    }
    for (id, step) in all_ended(events, &ids, OP).await {
        assert_eq!(step, "done", "create of {id}: {:#?}", events.seen);
    }
    ids
}

/// Runs `op` (`start`, `stop`) on every id at once and returns how long it took until every one
/// ended `done`.
async fn all_at_once(api: &Api, events: &mut Events, ids: &[String], op: &str) -> Duration {
    let started = Instant::now();
    let replies = futures_util::future::join_all(
        ids.iter()
            .map(|id| async move { api.post(&format!("/api/workspaces/{id}/{op}"), "").await }),
    )
    .await;
    for reply in &replies {
        assert_eq!(reply.status, 202, "{}", reply.body);
    }
    for (id, step) in all_ended(events, ids, OP).await {
        assert_eq!(step, "done", "{op} of {id}: {:#?}", events.seen);
    }
    started.elapsed()
}

/// Deletes the stopped workspace `id`. The image the delete check boots is downloaded when a
/// workspace stops, and a delete that comes before it is in answers with a retry message and leaves
/// the workspace as it was, so the request is repeated until it is accepted.
async fn delete_stopped(api: &Api, events: &mut Events, id: &str) {
    let confirm = json!({"confirm": true}).to_string();
    let path = format!("/api/workspaces/{id}");
    let (api_ref, path_ref, confirm_ref) = (api, &path, &confirm);
    let gone = eventually_async(
        "the delete is accepted once the check image is downloaded",
        Duration::from_secs(95),
        || async move {
            let reply = api_ref.delete(path_ref, Some(confirm_ref)).await;
            assert_ne!(reply.status, 408, "the request limit ended the delete");
            if reply.status != 503 {
                return Some(reply);
            }
            assert!(reply.body.contains("downloading"), "{}", reply.body);
            None
        },
    )
    .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    let end = events.until(ended(id), OP).await;
    assert_eq!(end["step"], "done", "{end}");
}

/// The workspaces the API lists, as `(id, name, status, busy)`.
async fn listed(api: &Api) -> Vec<(String, String, String, Value)> {
    let reply = api.get("/api/workspaces").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["id"].as_str().unwrap().to_owned(),
                w["name"].as_str().unwrap().to_owned(),
                w["status"].as_str().unwrap().to_owned(),
                w["busy"].clone(),
            )
        })
        .collect()
}

/// The workspace book on disk: its entries as `(id, name)` and its retired ids.
fn book_on_disk(root: &std::path::Path) -> (Vec<(String, String)>, BTreeSet<String>) {
    let path = puddle_host::HostPaths::new(root).workspace_book();
    let book: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let entries = book["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["id"].as_str().unwrap().to_owned(),
                e["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let retired = book["retired_ids"]
        .as_array()
        .map(|ids| {
            ids.iter()
                .map(|id| id.as_str().unwrap().to_owned())
                .collect()
        })
        .unwrap_or_default();
    (entries, retired)
}

/// Every workspace listed, each exactly once, running and not busy.
fn assert_all_running(rows: &[(String, String, String, Value)], ids: &[String]) {
    let seen: BTreeSet<&str> = rows.iter().map(|r| r.0.as_str()).collect();
    assert_eq!(rows.len(), ids.len(), "{rows:?}");
    assert_eq!(seen, ids.iter().map(String::as_str).collect(), "{rows:?}");
    for (id, _, status, busy) in rows {
        assert_eq!(status, "running", "{id}: {rows:?}");
        assert_eq!(*busy, Value::Null, "{id} is still busy: {rows:?}");
    }
}

/// Creates every name in a row and returns the ids and how long each create took. Each ends
/// done; piling workspaces up does not slow a create down. One create on a loaded shared host can
/// take minutes (a single spike), so no single create is held to a ratio: the median of the last
/// four may not be more than a multiple of the median of the first four, and none may take longer
/// than a bound that only a hang reaches.
async fn create_in_a_row(
    api: &Api,
    events: &mut Events,
    names: &[String],
) -> (Vec<String>, Vec<Duration>) {
    let mut ids = Vec::new();
    let mut took = Vec::new();
    for name in names {
        let (id, elapsed) = create_one(api, events, name).await;
        eprintln!("create of {name} ({id}) took {elapsed:?}");
        assert!(
            elapsed <= CREATE_HANG,
            "the create of {name} took {elapsed:?}"
        );
        ids.push(id);
        took.push(elapsed);
    }
    let median = |part: &[Duration]| {
        let mut sorted = part.to_vec();
        sorted.sort();
        (sorted[1] + sorted[2]) / 2
    };
    let (early, late) = (median(&took[..4]), median(&took[4..]));
    assert!(
        late <= early * CREATE_SLOWDOWN,
        "creates five to eight took {late:?} at the median against {early:?} for the first four \
         (bound: {CREATE_SLOWDOWN} times): {took:?}"
    );
    (ids, took)
}

/// The book on disk and the runtime's volumes agree with the eight workspaces in use: each once,
/// none of them retired, every deleted id retired and without a volume, eight distinct boots.
async fn assert_book_and_runtime(vm: &vm_support::VmHost, current: &[String], old: &[String]) {
    let (entries, retired) = book_on_disk(&vm.root);
    let entry_ids: BTreeSet<&str> = entries.iter().map(|e| e.0.as_str()).collect();
    assert_eq!(entries.len(), 8, "no duplicate in the book: {entries:?}");
    assert_eq!(entry_ids, current.iter().map(String::as_str).collect());
    for id in old {
        assert!(retired.contains(id), "{id} is retired: {retired:?}");
    }
    for id in current {
        assert!(!retired.contains(id), "{id} is in use but retired");
    }
    let volumes: Vec<String> = vm
        .runtime
        .list_volumes()
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    for id in current {
        assert!(volumes.contains(&format!("ws-{id}")), "{id}: {volumes:?}");
    }
    for id in old {
        assert!(!volumes.contains(&format!("ws-{id}")), "{id}: {volumes:?}");
    }
    let mut boots = BTreeSet::new();
    for id in current {
        let sandbox = WorkspaceId::new(id).unwrap();
        boots.insert(
            guest(&vm.runtime, &sandbox, "cat /proc/sys/kernel/random/boot_id")
                .await
                .trim()
                .to_owned(),
        );
    }
    assert_eq!(boots.len(), 8, "eight machines, eight boots: {boots:?}");
}

/// Stops, deletes and makes again under the same names the first four workspaces, two at once
/// and then two at once. Returns the new ids of those four.
async fn remake_four(
    api: &Api,
    events: &mut Events,
    ids: &[String],
    names: &[String],
) -> Vec<String> {
    all_at_once(api, events, &ids[..4], "stop").await;
    for id in &ids[..4] {
        delete_stopped(api, events, id).await;
    }
    let mut again = create_together(api, events, &names[..2]).await;
    again.extend(create_together(api, events, &names[2..4]).await);
    again
}

/// All eight stop at once, then start at once: every one stopped and then running, none left busy.
async fn stop_and_start_together(
    vm: &vm_support::VmHost,
    api: &Api,
    events: &mut Events,
    current: &[String],
) {
    let stopped_in = all_at_once(api, events, current, "stop").await;
    eprintln!("eight stopped in {stopped_in:?}");
    assert!(stopped_in < STOP_ALL_BOUND, "{stopped_in:?}");
    for (id, _, status, busy) in &listed(api).await {
        assert_eq!(status, "stopped", "{id}");
        assert_eq!(*busy, Value::Null, "{id} is still busy");
    }
    let started_in = all_at_once(api, events, current, "start").await;
    eprintln!("eight started in {started_in:?}");
    assert!(started_in < START_ALL_BOUND, "{started_in:?}");
    assert_all_running(&listed(api).await, current);
    for id in current {
        let sandbox = WorkspaceId::new(id).unwrap();
        assert_eq!(guest(&vm.runtime, &sandbox, "echo up").await.trim(), "up");
    }
}

/// Eight workspaces through their life: made in a row (the later ones no slower than the early
/// ones), four deleted and their names made again in two concurrent pairs (every id new and none
/// a retired one), all eight stopped at once and started at once (every one running, none left
/// busy, within a bound), and a quit and a launch of Puddle (the same eight, each once, stopped).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_eight_workspaces_are_made_remade_started_together_and_listed_after_a_restart() {
    let vm = start_vm_host("eight").await;
    let prefix = vm.settings.prefix.as_str().to_owned();
    let names: Vec<String> = (1..=8).map(|i| format!("{prefix}-m{i}")).collect();
    let api = Api::new(vm.host.url(), vm.host.token());
    let mut events = api.events().await;
    let api = &api;

    let (ids, _took) = create_in_a_row(api, &mut events, &names).await;
    assert_eq!(
        ids.iter().collect::<BTreeSet<_>>().len(),
        8,
        "eight distinct ids: {ids:?}"
    );
    assert_all_running(&listed(api).await, &ids);

    let old: Vec<String> = ids[..4].to_vec();
    let again = remake_four(api, &mut events, &ids, &names).await;
    let mut current: Vec<String> = again.clone();
    current.extend(ids[4..].iter().cloned());
    assert_eq!(
        current.iter().collect::<BTreeSet<_>>().len(),
        8,
        "eight distinct ids after the re-creates: {current:?}"
    );
    for id in &again {
        assert!(!old.contains(id), "{id} is an id of a deleted workspace");
    }
    let rows = listed(api).await;
    assert_all_running(&rows, &current);
    let rows_names: BTreeSet<&str> = rows.iter().map(|r| r.1.as_str()).collect();
    assert_eq!(
        rows_names,
        names.iter().map(String::as_str).collect(),
        "the names are the same eight, each once: {rows:?}"
    );
    assert_book_and_runtime(&vm, &current, &old).await;

    stop_and_start_together(&vm, api, &mut events, &current).await;

    // A quit with all eight running and a launch: the same eight, each once, all stopped.
    let before: BTreeSet<(String, String)> =
        rows.iter().map(|r| (r.0.clone(), r.1.clone())).collect();
    drop(events);
    let vm = Box::pin(vm_support::restart_vm_host(vm)).await;
    let api = Api::new(vm.host.url(), vm.host.token());
    let after = listed(&api).await;
    assert_eq!(after.len(), 8, "exactly eight entries: {after:?}");
    let after_pairs: BTreeSet<(String, String)> =
        after.iter().map(|r| (r.0.clone(), r.1.clone())).collect();
    assert_eq!(after_pairs, before, "the same ids and names");
    for (id, _, status, busy) in &after {
        assert_eq!(status, "stopped", "{id}: {after:?}");
        assert_eq!(*busy, Value::Null, "{id} is busy after the launch");
    }
    let (entries, _) = book_on_disk(&vm.root);
    assert_eq!(entries.len(), 8, "no duplicate in the book: {entries:?}");

    let down = vm.host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
