// SPDX-License-Identifier: AGPL-3.0-or-later
//! Root disk size, growth and reset.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn the_list_and_one_workspace_say_what_the_volume_takes_when_the_runtime_knows() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // A runtime that cannot say leaves the field empty: the card shows the size alone.
    let list = api.get("/api/workspaces").await.json();
    assert!(list["workspaces"][0]["disk_used_mib"].is_null(), "{list}");

    rig.runtime.set_volume_used(
        &WorkspaceId::new("acme").unwrap().volume_name(),
        puddle_compute::DiskSize::mib(1536),
    );
    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["disk_used_mib"], 1536, "{list}");
    let one = api.get("/api/workspaces/acme").await.json();
    assert_eq!(one["disk_used_mib"], 1536, "{one}");

    // A runtime that cannot list its volumes does not stop the list: the last value stays.
    rig.runtime
        .inject(Op::ListVolumes, Fault::once(disk_error("list volumes")));
    let list = api.get("/api/workspaces").await;
    assert_eq!(list.status, 200, "{}", list.body);
    assert_eq!(list.json()["workspaces"][0]["disk_used_mib"], 1536);

    // A workspace's own answer follows an operation without a list being asked for.
    rig.runtime.set_volume_used(
        &WorkspaceId::new("acme").unwrap().volume_name(),
        puddle_compute::DiskSize::mib(1700),
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let one = api.get("/api/workspaces/acme").await.json();
    assert_eq!(one["disk_used_mib"], 1700, "{one}");
    host.shutdown().await;
}

async fn put_settings(api: &Api, body: serde_json::Value) -> support::Reply {
    api.request("PUT", "/api/settings", Some(&body.to_string()), true)
        .await
}

fn disk_warning(kind: &'static str) -> impl Fn(&serde_json::Value) -> bool {
    move |e| e["type"] == "disk_nearly_full" && e["disk"] == kind
}

#[tokio::test(flavor = "multi_thread")]
async fn a_root_disk_at_the_threshold_is_flagged_and_announced_once_until_it_drops_below() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = WorkspaceId::new("acme").unwrap().sandbox_name();
    let list = || async { api.get("/api/workspaces").await.json()["workspaces"][0].clone() };

    // Below the default 95 %: the figures show, nothing is flagged.
    rig.runtime.set_root_disk_use(
        &sandbox,
        puddle_compute::DiskSize::mib(4096),
        puddle_compute::DiskSize::mib(3800),
    );
    let w = list().await;
    assert_eq!(w["root_disk_size_mib"], 4096, "{w}");
    assert_eq!(w["root_disk_used_mib"], 3800, "{w}");
    assert_eq!(w["disk_warnings"], json!([]), "{w}");

    // At 95 % it is flagged, and the event says which disk and how full.
    rig.runtime.set_root_disk_use(
        &sandbox,
        puddle_compute::DiskSize::mib(4096),
        puddle_compute::DiskSize::mib(3900),
    );
    assert_eq!(list().await["disk_warnings"], json!(["root"]));
    let said = events
        .until(disk_warning("root"), Duration::from_secs(20))
        .await;
    assert_eq!(
        (said["workspace"].clone(), said["percent"].clone()),
        (json!("acme"), json!(95))
    );

    // Reading again says nothing new; the volume is far below the threshold.
    let from = events.seen.len();
    list().await;
    list().await;
    assert!(
        events.seen[from..]
            .iter()
            .all(|e| e["type"] != "disk_nearly_full")
    );

    // Below the threshold re-arms the warning.
    rig.runtime.set_root_disk_use(
        &sandbox,
        puddle_compute::DiskSize::mib(4096),
        puddle_compute::DiskSize::mib(100),
    );
    assert_eq!(list().await["disk_warnings"], json!([]));
    rig.runtime.set_root_disk_use(
        &sandbox,
        puddle_compute::DiskSize::mib(4096),
        puddle_compute::DiskSize::mib(4096),
    );
    list().await;
    let again = events
        .until(disk_warning("root"), Duration::from_secs(20))
        .await;
    assert_eq!(again["percent"], 100);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_that_cannot_list_leaves_the_last_root_disk_figures() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = WorkspaceId::new("acme").unwrap().sandbox_name();
    rig.runtime.set_root_disk_use(
        &sandbox,
        puddle_compute::DiskSize::mib(4096),
        puddle_compute::DiskSize::mib(3800),
    );
    let list = || async { api.get("/api/workspaces").await.json()["workspaces"][0].clone() };
    assert_eq!(list().await["root_disk_used_mib"], 3800);
    rig.runtime
        .inject(Op::List, Fault::always(disk_error("list")));
    assert_eq!(list().await["root_disk_used_mib"], 3800);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_volume_is_flagged_and_the_threshold_is_a_validated_setting() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let volume = WorkspaceId::new("acme").unwrap().volume_name();
    let size = rig.runtime.volume(&volume).await.unwrap().unwrap().size;
    let used = |percent: u32| puddle_compute::DiskSize::mib(size.as_mib() / 100 * percent);

    rig.runtime.set_volume_used(&volume, used(96));
    let w = api.get("/api/workspaces/acme").await;
    assert_eq!(
        w.json()["disk_warnings"],
        json!([]),
        "get reads the last refresh"
    );
    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["disk_warnings"], json!(["volume"]));
    events
        .until(disk_warning("volume"), Duration::from_secs(20))
        .await;

    // A higher threshold takes the flag away; one outside 50 to 100 is refused.
    let ok = put_settings(&api, json!({"ui": {"disk_warning_percent": 99}})).await;
    assert_eq!(ok.status, 200, "{}", ok.body);
    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["disk_warnings"], json!([]), "{list}");
    for bad in [49, 101] {
        let refused = put_settings(&api, json!({"ui": {"disk_warning_percent": bad}})).await;
        assert_eq!(refused.status, 422, "{bad}: {}", refused.body);
    }
    let kept = api.get("/api/settings").await.json();
    assert_eq!(kept["ui"]["disk_warning_percent"], 99, "{kept}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn resetting_the_root_disk_needs_a_stopped_workspace_and_keeps_the_volume() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = WorkspaceId::new("acme").unwrap().sandbox_name();
    let volume = WorkspaceId::new("acme").unwrap().volume_name();

    let running = api.post("/api/workspaces/acme/reset-root-disk", "").await;
    assert_eq!(running.status, 409, "{}", running.body);
    assert!(running.body.contains("stop it"), "{}", running.body);

    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let reply = api.post("/api/workspaces/acme/reset-root-disk", "").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "resetting_root_disk");
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done");
    assert!(
        rig.runtime
            .list()
            .await
            .unwrap()
            .iter()
            .all(|s| s.name != sandbox.as_str()),
        "the sandbox, and with it the root disk, is gone"
    );
    assert!(
        rig.runtime.volume(&volume).await.unwrap().is_some(),
        "the volume stays"
    );
    let w = api.get("/api/workspaces/acme").await.json();
    assert!(w["root_disk_used_mib"].is_null(), "{w}");
    assert_eq!(w["status"], "stopped", "{w}");

    // A reset with no sandbox left has nothing to remove and still ends well.
    let again = api.post("/api/workspaces/acme/reset-root-disk", "").await;
    assert_eq!(again.status, 202, "{}", again.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done");

    // The next start makes a fresh sandbox from the image.
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        rig.runtime
            .list()
            .await
            .unwrap()
            .iter()
            .any(|s| s.name == sandbox.as_str())
    );
    host.shutdown().await;
}

/// The last `root_disk_grown` record of the activity log: (outcome, detail).
async fn grown_record(api: &Api) -> (String, String) {
    let ops = workspace_ops("workspace=acme", api).await;
    // No record fails the callers' outcome assertion, which prints this detail.
    ops.iter()
        .rev()
        .find(|r| r["op"] == "root_disk_grown")
        .map_or_else(
            || (String::new(), "no root_disk_grown record".to_owned()),
            |last| {
                (
                    last["outcome"].as_str().unwrap().to_owned(),
                    last["detail"].as_str().unwrap().to_owned(),
                )
            },
        )
}

#[tokio::test(flavor = "multi_thread")]
async fn a_running_workspaces_root_disk_grows_live_and_the_audit_log_says_so() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    rig.runtime
        .set_root_disk_use(&sandbox, gib(16), puddle_compute::DiskSize::mib(100));
    let end = grow_root_disk(&api, &mut events, 20_480).await;
    assert_eq!(end["step"], "done", "{end}");
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["root_disk_size_mib"], 20_480, "{w}");
    assert_eq!(w["status"], "running", "{w}");
    assert_eq!(
        rig.runtime.root_disk_size(&sandbox),
        Some(puddle_compute::DiskSize::mib(20_480))
    );
    assert!(
        rig.runtime.calls().iter().all(|c| c.op != Op::Stop),
        "a live grow does not stop the workspace"
    );
    let (outcome, detail) = grown_record(&api).await;
    assert_eq!(outcome, "succeeded");
    assert_eq!(
        detail,
        "root disk grown from 16384 MiB to 20480 MiB, while it ran"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_root_disk_never_shrinks_and_never_grows_past_the_free_space() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    rig.runtime
        .set_root_disk_use(&sandbox, gib(16), puddle_compute::DiskSize::mib(100));
    // The host reads the figures again when an operation ends.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["root_disk_size_mib"], 16_384, "{w}");
    for size in [1_u64, 16_384] {
        let reply = api
            .post(
                "/api/workspaces/acme/grow-root-disk",
                &format!(r#"{{"size_mib": {size}}}"#),
            )
            .await;
        assert_eq!(reply.status, 422, "{}", reply.body);
        assert!(reply.body.contains("only grows"), "{}", reply.body);
    }
    let huge = api
        .post(
            "/api/workspaces/acme/grow-root-disk",
            &format!(r#"{{"size_mib": {}}}"#, u32::MAX),
        )
        .await;
    assert_eq!(huge.status, 422, "{}", huge.body);
    assert!(huge.body.contains("free"), "{}", huge.body);
    let missing = api
        .post(
            "/api/workspaces/nope/grow-root-disk",
            r#"{"size_mib": 20000}"#,
        )
        .await;
    assert_eq!(missing.status, 404, "{}", missing.body);
    assert!(
        workspace_ops("workspace=acme", &api)
            .await
            .iter()
            .all(|r| r["op"] != "root_disk_grown"),
        "a refused request is not a grow"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_grow_that_adds_most_of_the_free_space_is_allowed_with_a_warning() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let free_mib = support::FIXED_FREE_BYTES / (1024 * 1024);
    let size = 16_384 + free_mib * 3 / 4;
    let end = grow_root_disk(&api, &mut events, size).await;
    assert_eq!(end["step"], "done", "{end}");
    let problems = problems_of(&api).await;
    assert!(
        problems
            .iter()
            .any(|(key, ws, title, _)| key == "root-room:acme"
                && ws.as_deref() == Some("acme")
                && title.contains("bigger than most of the free space")),
        "{problems:?}"
    );
    // A small grow after it takes the warning away.
    let end = grow_root_disk(&api, &mut events, size + 1).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(
        problems_of(&api)
            .await
            .iter()
            .all(|(key, ..)| key != "root-room:acme")
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspaces_root_disk_grows_at_the_next_start_with_a_warning_and_a_reset_keeps_the_size()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let end = grow_root_disk(&api, &mut events, 24_576).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        rig.runtime.root_disk_size(&sandbox),
        Some(puddle_compute::DiskSize::mib(24_576))
    );
    let (_, detail) = grown_record(&api).await;
    assert_eq!(
        detail,
        "root disk grown from 16384 MiB to 24576 MiB, at its next start"
    );
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["status"], "stopped", "{w}");
    assert!(
        problems_of(&api)
            .await
            .iter()
            .any(|(key, _, title, _)| key == "root-copy:acme" && title.contains("old root disk")),
    );

    // A reset removes the sandbox and the warning; the fresh root disk is the grown size.
    let reply = api.post("/api/workspaces/acme/reset-root-disk", "").await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        problems_of(&api)
            .await
            .iter()
            .all(|(key, ..)| key != "root-copy:acme")
    );
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(
        rig.runtime.root_disk_size(&sandbox),
        Some(puddle_compute::DiskSize::mib(24_576))
    );

    // The size is in the saved list, so a new Puddle makes it again after a reset.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    host.shutdown().await;
    drop(host);
    drop(events);
    let again = rig.start().await;
    let api = Api::new(again.url(), again.token());
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/reset-root-disk", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(
        rig.runtime.root_disk_size(&sandbox),
        Some(puddle_compute::DiskSize::mib(24_576))
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_runtime_that_cannot_grow_live_gets_a_restart_that_loses_nothing() {
    let rig = Rig::named_ids();
    rig.runtime.set_live_root_grow(false);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    let end = grow_root_disk(&api, &mut events, 20_480).await;
    assert_eq!(end["step"], "done", "{end}");
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["status"], "running", "{w}");
    assert_eq!(
        rig.runtime.root_disk_size(&sandbox),
        Some(puddle_compute::DiskSize::mib(20_480))
    );
    let ops: Vec<Op> = rig.runtime.calls().iter().map(|c| c.op).collect();
    assert!(
        ops.contains(&Op::Stop) && ops.iter().filter(|o| **o == Op::Start).count() == 1,
        "{ops:?}"
    );
    let (outcome, detail) = grown_record(&api).await;
    assert_eq!(outcome, "succeeded");
    assert_eq!(
        detail,
        "root disk grown from 16384 MiB to 20480 MiB, with a restart"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_grow_changes_nothing_and_is_audited() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    rig.runtime.inject(
        Op::GrowRootDisk,
        Fault::once(puddle_compute::ComputeError::Runtime {
            op: "grow",
            message: "disk full".into(),
        }),
    );
    let end = grow_root_disk(&api, &mut events, 20_480).await;
    assert_eq!(end["step"], "failed", "{end}");
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["status"], "running", "{w}");
    assert_eq!(w["busy"], serde_json::Value::Null, "{w}");
    assert_eq!(rig.runtime.root_disk_size(&sandbox), Some(gib(16)));
    let (outcome, detail) = grown_record(&api).await;
    assert_eq!(outcome, "failed");
    assert!(
        detail.starts_with("root disk not grown from 16384 MiB to 20480 MiB: "),
        "{detail}"
    );
    // The size was not kept: a reset makes the default one.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/reset-root-disk", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(rig.runtime.root_disk_size(&sandbox), Some(gib(16)));
    host.shutdown().await;
}

/// The disk size the runtime reports can lag a grow that applies at the next start; the size
/// Puddle saved does not. A request is measured against the larger, so one between the two is
/// refused up front and not accepted with a 202 and refused by the runtime afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_grow_is_measured_against_the_saved_size_when_the_reported_one_lags() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let end = grow_root_disk(&api, &mut events, 24_576).await;
    assert_eq!(end["step"], "done", "{end}");
    // The runtime still reports the old disk until the next start.
    rig.runtime
        .set_root_disk_use(&sandbox, gib(16), puddle_compute::DiskSize::mib(100));
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    rig.runtime
        .set_root_disk_use(&sandbox, gib(16), puddle_compute::DiskSize::mib(100));
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let between = api
        .post(
            "/api/workspaces/acme/grow-root-disk",
            r#"{"size_mib": 18432}"#,
        )
        .await;
    assert_eq!(between.status, 422, "{}", between.body);
    assert!(between.body.contains("only grows"), "{}", between.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_for_a_grow_that_fails_leaves_the_workspace_as_a_stop_or_a_start_would() {
    let rig = Rig::named_ids();
    rig.runtime.set_live_root_grow(false);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // The stop fails: the workspace is still running.
    rig.runtime.inject(
        Op::Stop,
        Fault::once(puddle_compute::ComputeError::Runtime {
            op: "stop",
            message: "stuck".into(),
        }),
    );
    let end = grow_root_disk(&api, &mut events, 20_480).await;
    assert_eq!(end["step"], "failed", "{end}");
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["status"], "running", "{w}");
    // The grow was saved and the runtime applies it at the next start, so the log calls it done
    // and names the failed restart on its own.
    let (outcome, detail) = grown_record(&api).await;
    assert_eq!(outcome, "succeeded", "{detail}");
    assert!(
        detail.starts_with(
            "root disk grown from 16384 MiB to 20480 MiB, with a restart; the restart failed: "
        ) && detail.contains("stuck"),
        "{detail}"
    );
    // The start fails: the workspace is crashed, and a start brings it back.
    rig.runtime.inject(
        Op::Start,
        Fault::once(puddle_compute::ComputeError::Runtime {
            op: "start",
            message: "no memory".into(),
        }),
    );
    let end = grow_root_disk(&api, &mut events, 24_576).await;
    assert_eq!(end["step"], "failed", "{end}");
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["status"], "crashed", "{w}");
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let w = api.get("/api/workspaces/acme").await.json();
    assert_eq!(w["status"], "running", "{w}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_with_no_sandbox_yet_keeps_the_grown_size_for_its_first_start() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime
        .remove(&SandboxName::new("acme").unwrap())
        .await
        .unwrap();
    let end = grow_root_disk(&api, &mut events, 20_480).await;
    assert_eq!(end["step"], "done", "{end}");
    let (outcome, detail) = grown_record(&api).await;
    assert_eq!(outcome, "succeeded");
    assert_eq!(
        detail,
        "root disk grown from 16384 MiB to 20480 MiB, for the first start"
    );
    host.shutdown().await;
}
