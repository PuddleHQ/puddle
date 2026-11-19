// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restarts that keep sandboxes, and crashed or orphaned VMs.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_workspaces_and_the_sandbox_with_its_root_disk() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    let created_at = api1.get("/api/workspaces/acme").await.json()["created_at"].clone();
    let route = rig.runtime.routes(&SandboxName::new("acme").unwrap());
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    drop(events);

    let again = rig.start().await;
    let api2 = api(&again);
    let list = api2.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["name"], "acme");
    assert_eq!(list["workspaces"][0]["status"], "stopped");
    assert_eq!(list["workspaces"][0]["created_at"], created_at);
    // Reconcile kept the sandbox and the volume (the workspace is known).
    assert!(
        again.reconcile_report().removed.is_empty(),
        "{:?}",
        again.reconcile_report()
    );
    assert!(again.reconcile_report().volumes_removed.is_empty());

    // This process serves the route the sandbox was made with, so starting keeps the sandbox, and
    // with it the root disk: no new sandbox, none removed, the same route, no notice.
    let mut events = api2.events().await;
    api2.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        status_of(&api2.get("/api/workspaces/acme").await),
        "running"
    );
    assert_eq!(calls_of(&rig, "Create"), 1);
    assert_eq!(calls_of(&rig, "Remove"), 0);
    assert_eq!(
        rig.runtime.routes(&SandboxName::new("acme").unwrap()),
        route
    );
    let said = events.seen.iter().any(|e| {
        e["type"] == "workspace_progress"
            && e["detail"]
                .as_str()
                .is_some_and(|d| d.contains("made again"))
    });
    assert!(!said, "a kept sandbox needs no warning");
    assert_eq!(rig.runtime.list_volumes().await.unwrap().len(), 1);

    // The workspace works after the takeover: it stops and starts again in the same sandbox.
    api2.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api2.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(calls_of(&rig, "Create"), 1);
    again.shutdown().await;
}

/// A changed variable and a restart together: the stopped sandbox of the earlier run takes the new
/// variables, and still keeps its root disk.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_with_a_changed_variable_writes_it_to_the_kept_sandbox() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    api1.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    put_env(
        &api1,
        "/api/env/EDITOR",
        json!({"kind": "plain", "value": "nano"}),
    )
    .await;
    host.shutdown().await;
    drop(host);
    drop(events);

    let again = rig.start().await;
    let api2 = api(&again);
    let mut events = api2.events().await;
    api2.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(calls_of(&rig, "Create"), 1);
    assert_eq!(calls_of(&rig, "Remove"), 0);
    assert_eq!(
        rig.guest
            .boot_envs
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .get("EDITOR"),
        Some("nano")
    );
    again.shutdown().await;
}

/// A sandbox whose route this run does not serve (its install id is gone, as after a copy of the
/// data folder or a sandbox from before routes were stable) is made again on the same volume,
/// and the user is told what that costs.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_that_cannot_serve_the_sandboxes_route_makes_it_again_and_says_so() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    drop(host);
    drop(events);
    // A new install id: every route name changes.
    std::fs::write(
        rig.dir.path().join("data/ipc-id"),
        "0123456789abcdef0123456789abcdef",
    )
    .unwrap();

    let again = rig.start().await;
    let api2 = api(&again);
    let mut events = api2.events().await;
    api2.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(calls_of(&rig, "Create"), 2);
    assert_eq!(calls_of(&rig, "Remove"), 1);
    let said = events.seen.iter().any(|e| {
        e["type"] == "workspace_progress"
            && e["detail"]
                .as_str()
                .is_some_and(|d| d.contains("made again") && d.contains("volume"))
    });
    assert!(said, "no notice that the sandbox was made again");
    assert_eq!(rig.runtime.list_volumes().await.unwrap().len(), 1);
    // The new sandbox has the new run's stable route: a second restart keeps it.
    again.shutdown().await;
    drop(again);
    drop(events);
    let third = rig.start().await;
    let api3 = api(&third);
    let mut events = api3.events().await;
    api3.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(calls_of(&rig, "Create"), 2);
    assert_eq!(calls_of(&rig, "Remove"), 1);
    third.shutdown().await;
}

/// Docker's data folder is a disk of the sandbox itself: every sandbox the host makes has one,
/// a stop and start keeps the sandbox (and so the disk), a rebuilt sandbox gets a fresh one, and
/// the workspace volume never carries it.
#[tokio::test(flavor = "multi_thread")]
async fn every_sandbox_has_its_own_20_gib_disk_for_dockers_data_and_the_volume_does_not() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    let docker_disk = |rig: &Rig| {
        rig.runtime
            .owned_disks(&sandbox)
            .into_iter()
            .map(|d| (d.guest.as_str().to_owned(), d.size))
            .collect::<Vec<_>>()
    };
    let expected = vec![(
        "/var/lib/docker".to_owned(),
        puddle_compute::DiskSize::gib(20),
    )];
    assert_eq!(docker_disk(&rig), expected);
    let volumes = rig.runtime.volume_mounts(&sandbox);
    assert_eq!(volumes.len(), 1, "{volumes:?}");
    assert!(
        volumes
            .iter()
            .all(|v| !v.guest.as_str().starts_with("/var/lib")),
        "{volumes:?}"
    );

    // A stop and start is the same sandbox with the same disk.
    for step in ["stop", "start"] {
        let reply = api1.post(&format!("/api/workspaces/acme/{step}"), "").await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        events.until(ended("acme"), Duration::from_secs(20)).await;
    }
    assert_eq!(docker_disk(&rig), expected);
    assert_eq!(
        rig.runtime
            .calls()
            .iter()
            .filter(|c| format!("{:?}", c.op) == "Create")
            .count(),
        1
    );

    // A new process rebuilds the sandbox, and the new one has the disk too.
    host.shutdown().await;
    drop(host);
    drop(events);
    let again = rig.start().await;
    let api2 = api(&again);
    let mut events = api2.events().await;
    api2.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(docker_disk(&rig), expected);
    again.shutdown().await;
}

/// A new workspace's sandbox is made with a 16 GiB root disk (sparse on the host), and a stop and
/// start keeps that sandbox (a start never names a size, so a sandbox made at 4 GiB stays there).
#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspaces_sandbox_gets_a_16_gib_root_disk_and_a_start_keeps_it() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let sandbox = SandboxName::new("acme").unwrap();
    assert_eq!(
        rig.runtime.root_disk_size(&sandbox),
        Some(puddle_compute::DiskSize::gib(16))
    );
    for step in ["stop", "start"] {
        let reply = api.post(&format!("/api/workspaces/acme/{step}"), "").await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        events.until(ended("acme"), Duration::from_secs(20)).await;
    }
    assert_eq!(
        rig.runtime.root_disk_size(&sandbox),
        Some(puddle_compute::DiskSize::gib(16))
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_crashed_sandbox_is_reported_crashed_and_start_recovers_it() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    assert!(rig.runtime.crash(&SandboxName::new("acme").unwrap()));
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);

    let again = rig.start().await;
    let api2 = api(&again);
    assert_eq!(
        status_of(&api2.get("/api/workspaces/acme").await),
        "crashed"
    );
    assert_eq!(again.reconcile_report().crashed.len(), 1);
    let mut events = api2.events().await;
    api2.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vm_that_dies_while_puddle_runs_shows_crashed_and_start_recovers_it() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "running");

    // The VM process dies (out of memory, a monitor crash) with no request to Puddle.
    assert!(rig.runtime.crash(&SandboxName::new("acme").unwrap()));
    // The list reads the sandboxes, as the host's pass every minute does.
    let listed = api.get("/api/workspaces").await.json()["workspaces"][0].clone();
    assert_eq!(listed["status"], "crashed", "{listed}");
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "crashed");
    events
        .until(
            |e| e["type"] == "status_changed" && e["status"] == "crashed",
            Duration::from_secs(5),
        )
        .await;

    // What the VM held is released, so a start works and the workspace runs again.
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "running");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_end_of_an_agent_session_finds_a_dead_vm_without_a_list() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let id = WorkspaceId::new("acme").unwrap();

    // The agent restarting ends a session too: the VM is up, so nothing changes.
    puddle_agent_proto::host::GuestWatcher::session_ended(host.workspaces().ports(), &id);
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "running");

    // The VM dies and its session ends: the host reads the sandboxes by itself, no list asked.
    assert!(rig.runtime.crash(&SandboxName::new("acme").unwrap()));
    puddle_agent_proto::host::GuestWatcher::session_ended(host.workspaces().ports(), &id);
    events
        .until(
            |e| e["type"] == "status_changed" && e["status"] == "crashed",
            Duration::from_secs(15),
        )
        .await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "crashed");
    // The re-reads stop at the next gap, now that the workspace no longer shows Running.
    tokio::time::sleep(Duration::from_secs(3)).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vm_left_running_by_a_dead_puddle_is_stopped_at_the_next_start() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    // The first host never shuts down (it died): its VM still runs.
    let running = rig.runtime.list().await.unwrap();
    assert!(!running[0].status.is_down());

    // It died: its hold on the data folder ended with it, and the VM kept running.
    drop(host);
    // Let whatever the drop ends finish before the VM is made to outlive it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        rig.runtime
            .outlive_owner(&SandboxName::new("acme").unwrap())
    );
    assert!(
        !rig.runtime
            .outlive_owner(&SandboxName::new("nobody").unwrap())
    );
    let again = rig.start().await;
    assert_eq!(
        again.reconcile_report().stopped.len(),
        1,
        "{:?}",
        again.reconcile_report()
    );
    assert!(rig.runtime.list().await.unwrap()[0].status.is_down());
    assert_eq!(
        status_of(&api(&again).get("/api/workspaces/acme").await),
        "stopped"
    );
    again.shutdown().await;
}

/// What became of a workspace's VM when Puddle was killed in the middle of stopping it.
#[derive(Debug, Clone, Copy)]
enum VmAfterKill {
    /// It died with Puddle (the watchdog or the job object ended it).
    Died,
    /// It went on running (no watchdog or job object ended it): the next start must stop it.
    LeftRunning,
}

/// Puddle is killed (no shutdown) while a stop runs, at the stop's first guest command. Whatever
/// the VM was left doing, the next start shows the workspace stopped or crashed, never draining,
/// and a start brings it back.
// Four workers whatever the CPU count: the held guest command blocks a worker (block_in_place) while the test's own
// requests need one; on the one CPU the nightly stress job pins to, the default of one worker starved them for 20 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn puddle_killed_during_a_stop_leaves_the_workspace_stopped_or_crashed_and_a_start_works() {
    for vm in [VmAfterKill::Died, VmAfterKill::LeftRunning] {
        let rig = Rig::named_ids();
        let host = rig.start().await;
        let api1 = api(&host);
        let mut events = api1.events().await;
        create(&api1, &mut events, "acme").await;

        let held = rig.guest.hold_next(&["trim", "shutdown-trim"]);
        let reply = api1.post("/api/workspaces/acme/stop", "").await;
        assert_eq!(reply.status, 202, "{vm:?}: {}", reply.body);
        held.wait_reached();
        assert_eq!(
            status_of(&api1.get("/api/workspaces/acme").await),
            "draining",
            "{vm:?}"
        );
        drop(events);
        drop(host);
        held.kill();
        rig.await_dead_host().await;
        let sandbox = SandboxName::new("acme").unwrap();
        let set = match vm {
            VmAfterKill::Died => rig.runtime.set_status(&sandbox, WorkspaceStatus::Crashed),
            VmAfterKill::LeftRunning => rig.runtime.outlive_owner(&sandbox),
        };
        assert!(set, "{vm:?}");

        let again = rig.start().await;
        let api2 = api(&again);
        let shown = status_of(&api2.get("/api/workspaces/acme").await);
        assert!(
            shown == "stopped" || shown == "crashed",
            "{vm:?}: the workspace shows {shown}"
        );
        let mut events = api2.events().await;
        let reply = api2.post("/api/workspaces/acme/start", "").await;
        assert_eq!(reply.status, 202, "{vm:?}: {}", reply.body);
        let end = events.until(ended("acme"), Duration::from_secs(20)).await;
        assert_eq!(end["step"], "done", "{vm:?}: {end}");
        assert_eq!(
            status_of(&api2.get("/api/workspaces/acme").await),
            "running",
            "{vm:?}"
        );
        again.shutdown().await;
    }
}
