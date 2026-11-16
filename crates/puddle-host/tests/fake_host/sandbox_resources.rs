// SPDX-License-Identifier: AGPL-3.0-or-later
//! Memory and CPU a workspace's sandbox is given.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspace_gets_the_memory_the_settings_name() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let put = api
        .request(
            "PUT",
            "/api/settings",
            Some(&json!({"workspace_defaults": {"memory": 2048}}).to_string()),
            true,
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    create(&api, &mut events, "acme").await;
    let spec_memory = api.get("/api/workspaces/acme").await.json()["memory_mib"].clone();
    assert_eq!(spec_memory, 2048);
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    // The settings are files: a second host over the same folder sees them.
    let again = rig.start().await;
    let reply = api_for(&again).get("/api/settings").await;
    assert!(reply.body.contains("2048"), "{}", reply.body);
    again.shutdown().await;
}

/// The CPUs the fake guest of `name` has online, as the fake's `/sys/devices/system/cpu/online`.
async fn online_cpus(rig: &Rig, name: &str) -> String {
    use puddle_compute::Sandbox as _;
    let sandbox = WorkspaceId::new(name).unwrap().sandbox_name();
    let out = rig
        .runtime
        .get(&sandbox)
        .await
        .unwrap()
        .exec(ExecRequest::new("cat", ["/sys/devices/system/cpu/online"]))
        .await
        .unwrap();
    out.stdout_text().trim().to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_boots_with_the_cpus_the_settings_name_and_a_change_applies_at_the_next_start()
{
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let global = api
        .request(
            "PUT",
            "/api/settings",
            Some(
                &json!({"workspace_defaults": {"cpus": 2}, "limits": {"max_cpus": 8}}).to_string(),
            ),
            true,
        )
        .await;
    assert_eq!(global.status, 200, "{}", global.body);
    create(&api, &mut events, "acme").await;
    assert_eq!(online_cpus(&rig, "acme").await, "0-1", "the global default");

    let own = api
        .request(
            "PUT",
            "/api/settings/workspaces/acme",
            Some(&json!({"overrides": {"cpus": 4}}).to_string()),
            true,
        )
        .await;
    assert_eq!(own.status, 200, "{}", own.body);
    assert_eq!(
        online_cpus(&rig, "acme").await,
        "0-1",
        "a running workspace keeps its CPUs"
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(online_cpus(&rig, "acme").await, "0-3", "its own override");

    // Lowering the global maximum under a stored count lowers the next start.
    let lowered = api
        .request(
            "PUT",
            "/api/settings",
            Some(
                &json!({"workspace_defaults": {"cpus": 2}, "limits": {"max_cpus": 2}}).to_string(),
            ),
            true,
        )
        .await;
    assert_eq!(lowered.status, 200, "{}", lowered.body);
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(
        online_cpus(&rig, "acme").await,
        "0-1",
        "capped by the maximum"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_needs_the_default_memory_is_refused_while_the_settings_are_unreadable() {
    let rig = Rig::named_ids();
    let file = damage_global_settings(&rig);
    let host = rig.start().await;
    let api = api(&host);
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("global.json"), "{message}");
    assert!(message.contains("choose a memory size"), "{message}");
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"],
        json!([])
    );

    // Naming the memory needs no settings, so that way out works.
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": "acme", "repositories": [{"url": REPO}], "memory_mib": 4096})
                .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let view = api.get("/api/workspaces/acme").await.json();
    assert_eq!(view["memory_mib"], 4096);
    // The same damage is on the workspace, and direct SSH stays off.
    assert_eq!(view["direct_ssh"], false);
    assert!(view["settings_error"].as_str().is_some(), "{view}");
    assert!(
        !host
            .workspaces()
            .direct_ssh_allowed(&WorkspaceId::new("acme").unwrap())
    );
    assert!(file.is_file(), "the damaged file is left for the user");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stored_memory_size_that_is_not_valid_stops_the_start_and_names_the_way_out() {
    // Below the minimum, zero, and above the maximum (1 TiB in MiB).
    for bad in [0_u32, 1, 1024 * 1024 + 1, u32::MAX] {
        let rig = Rig::new();
        let data = rig.dir.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(
            data.join("workspaces.json"),
            json!({"version": 1, "workspaces": [{
                "id": "acme", "name": "acme", "repo_url": REPO, "image": "img",
                "memory_mib": bad, "created_at": 0, "disk_size_mib": 64
            }]})
            .to_string(),
        )
        .unwrap();
        let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
        let err = Host::start(
            prepared,
            &FakeFactory::new(&rig.runtime, &rig.log),
            hermetic_options(),
        )
        .await
        .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("workspace acme"), "{bad}: {message}");
        assert!(message.contains("workspaces.json"), "{bad}: {message}");
        // Whatever reconcile looked at, nothing was removed: the volume of the workspace that
        // could not be listed is safe.
        let calls = rig.runtime.calls();
        assert!(
            calls
                .iter()
                .all(|c| !matches!(c.op, Op::Remove | Op::RemoveVolume)),
            "{bad}: {calls:?}"
        );
    }
}

fn api_for(host: &Host<FakeRuntime>) -> Api {
    api(host)
}
