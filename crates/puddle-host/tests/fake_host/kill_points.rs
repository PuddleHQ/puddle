// SPDX-License-Identifier: AGPL-3.0-or-later
//! The kill-point matrix: Puddle dies (no shutdown, no drop of a sandbox handle) at a named
//! guest command of each long operation, and the next start must report, show and keep what a
//! user can rely on: a workspace whose create never finished is gone with its sandbox and volume;
//! every other workspace keeps its volume and starts again. The stop is in
//! `restart_and_crash.rs`; a kill at a runtime call that runs no guest command (a stopped grow,
//! a reset of the root disk) needs a hold in the fake runtime and is not here.

use super::*;

/// What became of a VM when Puddle died next to it.
#[derive(Debug, Clone, Copy)]
enum Vm {
    /// It died with Puddle (the watchdog or the job object ended it).
    Died,
    /// It went on running: the next start must stop it.
    LeftRunning,
    /// It was still booting: the runtime refuses to connect to it.
    LeftStarting,
}

/// Ends the host the way a kill does and sets what became of every sandbox it left.
async fn kill(rig: &Rig, host: Host<FakeRuntime>, events: support::Events, held: Held, vm: Vm) {
    drop(events);
    drop(host);
    held.kill();
    rig.await_dead_host().await;
    leave_vms(rig, vm).await;
}

async fn leave_vms(rig: &Rig, vm: Vm) {
    for s in rig.runtime.list().await.unwrap() {
        let name = SandboxName::new(&s.name).unwrap();
        let set = match vm {
            Vm::Died => rig.runtime.set_status(&name, WorkspaceStatus::Crashed),
            Vm::LeftRunning => rig.runtime.outlive_owner(&name),
            Vm::LeftStarting => rig.runtime.set_status(&name, WorkspaceStatus::Starting),
        };
        assert!(set, "{vm:?}: {name}");
    }
}

fn saved_book(rig: &Rig) -> serde_json::Value {
    let bytes = std::fs::read(rig.dir.path().join("data").join("workspaces.json")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn volume_of(id: &str) -> puddle_types::VolumeName {
    WorkspaceId::new(id).unwrap().volume_name()
}

/// Starts `id` through the API and waits for the end of the operation.
async fn start_ends_done(api: &Api, events: &mut support::Events, id: &'static str, why: &str) {
    let reply = api.post(&format!("/api/workspaces/{id}/start"), "").await;
    assert_eq!(reply.status, 202, "{why}: {}", reply.body);
    let end = events.until(ended(id), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{why}: {end}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn puddle_killed_during_a_create_leaves_no_workspace_sandbox_or_volume_and_retires_its_id() {
    for at in ["boot", "clone", "sync"] {
        for vm in [Vm::Died, Vm::LeftRunning] {
            let why = format!("killed at {at}, {vm:?}");
            let rig = Rig::named_ids();
            let host = rig.start().await;
            let api1 = api(&host);
            let events = api1.events().await;
            let kinds: &'static [&'static str] = match at {
                "boot" => &["boot"],
                "clone" => &["clone"],
                _ => &["sync"],
            };
            let held = rig.guest.hold_next(kinds);
            let reply = api1.post("/api/workspaces", &new_workspace("acme")).await;
            assert_eq!(reply.status, 202, "{why}: {}", reply.body);
            held.wait_reached();
            kill(&rig, host, events, held, vm).await;

            let again = rig.start().await;
            let list = api(&again).get("/api/workspaces").await.json();
            let listed = list["workspaces"].as_array().unwrap();
            assert!(listed.is_empty(), "{why}: still listed: {list}");
            let report = again.reconcile_report();
            assert!(report.failures.is_empty(), "{why}: {report:?}");
            assert_eq!(saved_book(&rig)["retired_ids"], json!(["acme"]), "{why}");
            let sandboxes = rig.runtime.list().await.unwrap();
            assert!(sandboxes.is_empty(), "{why}: sandboxes left: {sandboxes:?}");
            let volumes = rig.runtime.list_volumes().await.unwrap();
            assert!(volumes.is_empty(), "{why}: volumes left: {volumes:?}");
            again.shutdown().await;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn puddle_killed_during_a_start_keeps_the_workspace_and_its_volume_and_a_start_works() {
    for vm in [Vm::Died, Vm::LeftRunning, Vm::LeftStarting] {
        let why = format!("start killed at boot, {vm:?}");
        let rig = Rig::named_ids();
        let host = rig.start().await;
        let api1 = api(&host);
        let mut events = api1.events().await;
        create(&api1, &mut events, "acme").await;
        stop_and_wait(&api1, &mut events, "acme").await;

        let held = rig.guest.hold_next(&["boot"]);
        let reply = api1.post("/api/workspaces/acme/start", "").await;
        assert_eq!(reply.status, 202, "{why}: {}", reply.body);
        held.wait_reached();
        kill(&rig, host, events, held, vm).await;

        let again = rig.start().await;
        let api2 = api(&again);
        let shown = status_of(&api2.get("/api/workspaces/acme").await);
        assert!(
            shown == "stopped" || shown == "crashed",
            "{why}: the workspace shows {shown}"
        );
        assert!(
            rig.runtime
                .volume(&volume_of("acme"))
                .await
                .unwrap()
                .is_some(),
            "{why}: the volume is gone"
        );
        let mut events = api2.events().await;
        start_ends_done(&api2, &mut events, "acme", &why).await;
        let creates = rig
            .runtime
            .calls()
            .iter()
            .filter(|c| c.op == Op::Create)
            .count();
        assert_eq!(creates, 1, "{why}: the start made a second sandbox");
        again.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn puddle_killed_during_the_delete_check_leaves_no_maintenance_sandbox_and_a_whole_workspace()
{
    for vm in [Vm::Died, Vm::LeftRunning] {
        let why = format!("delete killed at check, {vm:?}");
        let rig = Rig::named_ids();
        let host = rig.start().await;
        let api1 = api(&host);
        let mut events = api1.events().await;
        create(&api1, &mut events, "acme").await;
        stop_and_wait(&api1, &mut events, "acme").await;

        let held = rig.guest.hold_next(&["check"]);
        // The delete answers only after its check, so the request waits in its own task.
        let request = api(&host);
        let pending = tokio::spawn(async move {
            request
                .delete(
                    "/api/workspaces/acme",
                    Some(&json!({"confirm": true}).to_string()),
                )
                .await
        });
        held.wait_reached();
        kill(&rig, host, events, held, vm).await;
        pending.abort();

        let again = rig.start().await;
        let api2 = api(&again);
        let left = rig.runtime.list().await.unwrap();
        assert!(
            left.iter()
                .all(|s| !puddle_workspace::is_maintenance_name(&s.name)),
            "{why}: a maintenance sandbox is left: {left:?}"
        );
        assert!(again.reconcile_report().failures.is_empty(), "{why}");
        let shown = api2.get("/api/workspaces/acme").await;
        assert_eq!(
            shown.status, 200,
            "{why}: the workspace is gone: {}",
            shown.body
        );
        assert!(
            rig.runtime
                .volume(&volume_of("acme"))
                .await
                .unwrap()
                .is_some(),
            "{why}: the check had not touched the volume, and it is gone"
        );
        let shown = status_of(&shown);
        assert!(
            shown == "stopped" || shown == "crashed",
            "{why}: the workspace shows {shown}"
        );
        let mut events = api2.events().await;
        start_ends_done(&api2, &mut events, "acme", &why).await;
        again.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn puddle_killed_while_quitting_stops_what_it_left_running_and_keeps_every_volume() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    create(&api1, &mut events, "beta").await;

    let held = rig.guest.hold_next(&["shutdown-trim"]);
    // The quit runs in its own task: the kill unwinds the task that runs the held command, and
    // the test's task must go on.
    let quit = tokio::spawn(async move { host.shutdown().await });
    held.wait_reached();
    drop(events);
    held.kill();
    let _ended_or_killed = quit.await;
    rig.await_dead_host().await;
    leave_vms(&rig, Vm::LeftRunning).await;

    let again = rig.start().await;
    let api2 = api(&again);
    let report = again.reconcile_report();
    assert!(report.failures.is_empty(), "{report:?}");
    for id in ["acme", "beta"] {
        let name = SandboxName::new(id).unwrap();
        assert!(
            report.stopped.contains(&name),
            "{id} not stopped: {report:?}"
        );
        assert_eq!(
            status_of(&api2.get(&format!("/api/workspaces/{id}")).await),
            "stopped"
        );
        assert!(
            rig.runtime.volume(&volume_of(id)).await.unwrap().is_some(),
            "{id}: the volume is gone"
        );
    }
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn puddle_killed_during_the_restart_of_a_grow_keeps_the_volume_and_a_start_works() {
    for vm in [Vm::Died, Vm::LeftRunning] {
        let why = format!("grow killed at its restart's stop, {vm:?}");
        let rig = Rig::named_ids();
        rig.runtime.set_live_root_grow(false);
        let host = rig.start().await;
        let api1 = api(&host);
        let mut events = api1.events().await;
        create(&api1, &mut events, "acme").await;

        let held = rig.guest.hold_next(&["trim", "shutdown-trim"]);
        let reply = api1
            .post(
                "/api/workspaces/acme/grow-root-disk",
                r#"{"size_mib": 20480}"#,
            )
            .await;
        assert_eq!(reply.status, 202, "{why}: {}", reply.body);
        held.wait_reached();
        kill(&rig, host, events, held, vm).await;

        let again = rig.start().await;
        let api2 = api(&again);
        let shown = status_of(&api2.get("/api/workspaces/acme").await);
        assert!(
            shown == "stopped" || shown == "crashed",
            "{why}: the workspace shows {shown}"
        );
        assert!(
            rig.runtime
                .volume(&volume_of("acme"))
                .await
                .unwrap()
                .is_some(),
            "{why}: the volume is gone"
        );
        let mut events = api2.events().await;
        start_ends_done(&api2, &mut events, "acme", &why).await;
        assert_eq!(
            rig.runtime
                .root_disk_size(&SandboxName::new("acme").unwrap()),
            Some(puddle_compute::DiskSize::mib(20_480)),
            "{why}: the grow the runtime accepted before the kill is kept"
        );
        again.shutdown().await;
    }
}
