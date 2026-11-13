// SPDX-License-Identifier: AGPL-3.0-or-later
//! The first-run flow of a fresh install on a real host and runtime (tiers K and W), through the
//! API the app window talks to: the system check boots a test machine (`GET /api/doctor`), then
//! three workspaces are created at once, each is its own running machine, and all three stop at once.
#![expect(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "VM test: a failed step fails the test; helpers outside #[test] fns panic"
)]

mod support;
mod vm_support;

use std::collections::BTreeSet;
use std::time::Duration;

use puddle_compute::Runtime;
use puddle_test_kit::eventually_async;
use puddle_types::WorkspaceId;
use serde_json::{Value, json};
use support::{Api, Events, ended};
use vm_support::{VmHost, guest, start_vm_host};

/// How long the test boot of the system check may take: the route's own documentation says up to
/// about 30 s.
const DOCTOR_BAR: Duration = Duration::from_secs(30);

/// How long one batch of workspace operations may take.
const BATCH: Duration = Duration::from_secs(600);

/// The status of check `id` in a doctor report, or `None` when the report has no such check.
fn check_status<'a>(report: &'a Value, id: &str) -> Option<&'a str> {
    report["checks"]
        .as_array()?
        .iter()
        .find(|check| check["id"] == id)
        .and_then(|check| check["status"].as_str())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_doctor_through_the_api_boots_a_test_machine_and_needs_the_token() {
    let VmHost { host, .. } = start_vm_host("doctor").await;
    let api = Api::new(host.url(), host.token());

    let refused = api.request("GET", "/api/doctor", None, false).await;
    assert_eq!(refused.status, 401, "{}", refused.body);

    let reply = api.get("/api/doctor").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let report = reply.json();
    for id in ["hypervisor", "runtime", "launch", "test_boot"] {
        assert_eq!(check_status(&report, id), Some("ok"), "{id}: {report:#}");
    }
    let took = Duration::from_millis(report["elapsed_ms"].as_u64().unwrap());
    assert!(took < DOCTOR_BAR, "{took:?}");

    // Without the test boot the report has no boot to run: the check is skipped.
    let quick = api.get("/api/doctor?boot=false").await;
    assert_eq!(quick.status, 200, "{}", quick.body);
    assert_eq!(
        check_status(&quick.json(), "test_boot"),
        Some("skipped"),
        "{}",
        quick.body
    );

    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// Reads events until every workspace of `names` has ended an operation, and returns the step each
/// ended with. One operation per workspace: the event stream keeps what it reads, so the end of one
/// workspace that arrives while another is awaited is not lost.
async fn all_ended(events: &mut Events, names: &[String]) -> Vec<(String, String)> {
    let any = |event: &Value| names.iter().any(|name| ended(name)(event));
    let mut done: Vec<(String, String)> = Vec::new();
    while done.len() < names.len() {
        let event = events.until(any, BATCH).await;
        let name = event["workspace"].as_str().unwrap().to_owned();
        if !done.iter().any(|(n, _)| *n == name) {
            done.push((name, event["step"].as_str().unwrap().to_owned()));
        }
    }
    done
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_three_workspaces_created_at_once_are_three_machines_and_stop_at_once() {
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host("three").await;
    let names: Vec<String> = ["one", "two", "three"]
        .iter()
        .map(|n| format!("{}-{n}", settings.prefix.as_str()))
        .collect();
    let api = Api::new(host.url(), host.token());
    let mut events = api.events().await;
    let api = &api;

    // All three requests are in flight before any answers.
    let replies = futures_util::future::join_all(names.iter().map(|name| async move {
        let body =
            json!({"name": name, "repositories": [], "image": "alpine:3", "memory_mib": 512});
        api.post("/api/workspaces", &body.to_string()).await
    }))
    .await;
    for reply in &replies {
        assert_eq!(reply.status, 202, "{}", reply.body);
    }
    for (name, step) in all_ended(&mut events, &names).await {
        assert_eq!(step, "done", "{name}: {:#?}", events.seen);
    }

    // Three running workspaces, each its own machine: its own boot and its own disk.
    let mut boots = BTreeSet::new();
    for name in &names {
        let workspace = api.get(&format!("/api/workspaces/{name}")).await.json();
        assert_eq!(workspace["status"], "running", "{workspace}");
        let sandbox = WorkspaceId::new(name).unwrap();
        guest(&runtime, &sandbox, &format!("echo {name} > /root/who")).await;
        boots.insert(
            guest(&runtime, &sandbox, "cat /proc/sys/kernel/random/boot_id")
                .await
                .trim()
                .to_owned(),
        );
    }
    assert_eq!(boots.len(), names.len(), "three machines, three boots");
    for name in &names {
        let sandbox = WorkspaceId::new(name).unwrap();
        assert_eq!(
            guest(&runtime, &sandbox, "cat /root/who").await.trim(),
            name,
            "a workspace sees its own disk and no other"
        );
    }

    // All three stop at once.
    let replies =
        futures_util::future::join_all(names.iter().map(|name| async move {
            api.post(&format!("/api/workspaces/{name}/stop"), "").await
        }))
        .await;
    for reply in &replies {
        assert_eq!(reply.status, 202, "{}", reply.body);
    }
    for (name, step) in all_ended(&mut events, &names).await {
        assert_eq!(step, "done", "stop of {name}: {:#?}", events.seen);
    }
    for name in &names {
        let workspace = api.get(&format!("/api/workspaces/{name}")).await.json();
        assert_eq!(workspace["status"], "stopped", "{workspace}");
    }
    let listed = api.get("/api/workspaces").await.json();
    for name in &names {
        assert!(
            listed["workspaces"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["name"] == name.as_str() && w["status"] == "stopped"),
            "{name} is listed as stopped: {listed}"
        );
    }

    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}

/// A user stops a workspace and deletes it: the delete is accepted within the request limit, ends
/// `done`, and leaves no sandbox and no volume of the workspace behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn vm_a_stopped_workspace_is_deleted_with_its_machine_and_disk() {
    let VmHost {
        host,
        runtime,
        settings,
        ..
    } = start_vm_host("delete").await;
    let name = format!("{}-gone", settings.prefix.as_str());
    let api = Api::new(host.url(), host.token());
    let mut events = api.events().await;

    let body = json!({"name": name, "repositories": [], "image": "alpine:3", "memory_mib": 512});
    let reply = api.post("/api/workspaces", &body.to_string()).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended(&name), BATCH).await;
    assert_eq!(end["step"], "done", "{end}");
    let stop = api.post(&format!("/api/workspaces/{name}/stop"), "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    let end = events.until(ended(&name), BATCH).await;
    assert_eq!(end["step"], "done", "{end}");

    // The image the delete check boots (not this workspace's own) is downloaded when the workspace
    // stops. A delete that comes before it is in answers with a retry message within the request
    // limit, never a 408, and leaves the workspace as it was.
    let confirm = json!({"confirm": true}).to_string();
    let path = format!("/api/workspaces/{name}");
    let (api_ref, path_ref, confirm_ref) = (&api, &path, &confirm);
    let gone = eventually_async(
        "the delete is accepted once the check image is downloaded",
        Duration::from_secs(95),
        || async move {
            let started = std::time::Instant::now();
            let reply = api_ref.delete(path_ref, Some(confirm_ref)).await;
            eprintln!(
                "delete answered {} after {:?}",
                reply.status,
                started.elapsed()
            );
            assert_ne!(reply.status, 408, "the request limit ended the delete");
            if reply.status != 503 {
                return Some(reply);
            }
            assert!(reply.body.contains("downloading"), "{}", reply.body);
            let workspace = api_ref.get(path_ref).await.json();
            assert_eq!(workspace["busy"], Value::Null, "{workspace}");
            None
        },
    )
    .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    let end = events.until(ended(&name), BATCH).await;
    assert_eq!(end["step"], "done", "{end}");

    assert_eq!(
        api.get(&format!("/api/workspaces/{name}")).await.status,
        404
    );
    let volumes = runtime.list_volumes().await.unwrap();
    assert!(
        volumes.iter().all(|v| !v.name.contains(&name)),
        "{volumes:?}"
    );
    let sandboxes = runtime.list().await.unwrap();
    assert!(
        sandboxes.iter().all(|s| !s.name.contains(&name)),
        "{sandboxes:?}"
    );

    let down = host.shutdown().await;
    assert!(down.sandboxes.all_stopped(), "{down:?}");
}
