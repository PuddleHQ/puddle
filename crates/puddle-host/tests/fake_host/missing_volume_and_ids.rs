// SPDX-License-Identifier: AGPL-3.0-or-later
//! Workspaces whose volume is gone, drawn ids and orphan records.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn starting_a_workspace_whose_volume_is_gone_refuses_and_makes_no_empty_volume() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("has no volume"), "{detail}");
    assert!(detail.contains("ws-acme"), "{detail}");
    assert!(detail.contains("no new empty volume"), "{detail}");
    // Nothing was made in its place.
    assert!(volume_names(&rig).await.is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());
    // The entry stays in the list (nothing else was lost).
    assert_eq!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_start_for_a_missing_volume_shows_the_volume_missing_state() {
    let (_rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["status"],
        "volume_missing"
    );
    // The state is a way to delete, and a start that finds the volume again leaves it.
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    assert_eq!(check["volume_missing"], true);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_shows_a_workspace_whose_volume_is_gone_as_volume_missing_before_any_start() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let shown = api.get("/api/workspaces/acme").await.json();
    assert_eq!(shown["status"], "volume_missing", "{shown}");
    let listed = api.get("/api/workspaces").await.json();
    assert_eq!(
        listed["workspaces"][0]["status"], "volume_missing",
        "{listed}"
    );
    // Only the start-up reads were made: nothing was created or removed for it.
    assert!(volume_names(&rig).await.is_empty());
    // The way out is the same as after a refused start: restore the volume, then Start.
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("acme").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_a_workspace_with_its_volume_stopped() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    drop(host);
    drop(events);
    let again = rig.start().await;
    let shown = api(&again).get("/api/workspaces/acme").await.json();
    assert_ne!(shown["status"], "volume_missing", "{shown}");
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_whose_volume_is_gone_can_be_deleted_without_a_check_in_a_sandbox() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    assert_eq!(check["volume_missing"], true, "{check}");
    assert_eq!(check["clean"], true, "{check}");
    let seen =
        json!({"confirm": true, "fingerprint": check["fingerprint"].as_str().unwrap()}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&seen)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(volume_names(&rig).await.is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_with_no_volume_also_removes_the_sandbox_the_restart_kept() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    // The restart kept the stopped sandbox record.
    assert_eq!(rig.runtime.list().await.unwrap().len(), 1);
    let api = api(&host);
    let mut events = api.events().await;
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    let seen = json!({"confirm": true, "fingerprint": check["fingerprint"]}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&seen)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(rig.runtime.list().await.unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restored_volume_lets_a_volume_missing_workspace_start() {
    let (rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("acme").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["status"],
        "running"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drawn_id_that_anything_is_kept_under_is_drawn_again_and_what_is_kept_stays() {
    let rig = Rig::named_ids();
    let paths = HostPaths::new(rig.dir.path().join("data"));
    let id = |s: &str| WorkspaceId::new(s).unwrap();
    // Before the start: a kept volume, a key half, a settings file, a login marker and a
    // retired id in the list; browser data the shell keeps.
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: id("vol").volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let key = paths.front_keys().join("key.key");
    let settings = paths.settings().join("workspaces").join("set.json");
    let marker = puddle_logins::Marker::for_workspace(&paths.login_markers(), &id("mark"));
    let marker = marker.path().unwrap().to_path_buf();
    for (file, text) in [
        (&key, "left"),
        (&settings, "{}"),
        (&marker, "puddle-logins-kept v2\n"),
        (
            &paths.workspace_book(),
            r#"{"version": 3, "workspaces": [], "retired_ids": ["old"]}"#,
        ),
    ] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
    }
    rig.launcher.data_under.lock().unwrap().push("web".into());
    let host = rig.start().await;
    // Sandboxes made after the start, which removes the ones it does not know.
    for name in ["sbx", "m--mnt"] {
        rig.runtime
            .create(puddle_compute::SandboxSpec::new(
                SandboxName::new(name).unwrap(),
                ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
            ))
            .await
            .unwrap();
    }
    let api = api(&host);
    let mut events = api.events().await;
    for name in ["vol", "sbx", "mnt", "key", "set", "mark", "web", "old"] {
        create_as(&api, &mut events, name, &format!("{name}-2")).await;
    }
    // Nothing that was kept was touched.
    assert!(volume_names(&rig).await.contains(&"ws-vol".to_owned()));
    assert_eq!(std::fs::read_to_string(&key).unwrap(), "left");
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{}");
    assert!(marker.is_file());
    assert!(
        !rig.launcher
            .forgotten
            .lock()
            .unwrap()
            .iter()
            .any(|id| id == "web" || id == "key")
    );
    let book: serde_json::Value =
        serde_json::from_slice(&std::fs::read(paths.workspace_book()).unwrap()).unwrap();
    assert_eq!(book["retired_ids"], json!(["old"]));
    host.shutdown().await;
}

/// A source that always gives the same id.
struct Always(&'static str);

impl puddle_api::IdSource for Always {
    fn draw(
        &self,
        _name: &puddle_api::DisplayName,
        _attempt: u32,
    ) -> Result<WorkspaceId, puddle_api::IdDrawError> {
        Ok(WorkspaceId::new(self.0).unwrap())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_source_that_keeps_giving_a_taken_id_gets_a_refusal_after_sixteen_draws() {
    let rig = Rig::new();
    *rig.ids.lock().unwrap() = Some(Arc::new(Always("k3m9q2xa")));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_as(&api, &mut events, "first", "k3m9q2xa").await;
    let reply = api.post("/api/workspaces", &new_workspace("second")).await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert_eq!(
        reply.json()["message"],
        "no free workspace id was found; the system's random source may be broken"
    );
    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"].as_array().unwrap().len(), 1, "{list}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_made_again_under_a_deleted_ones_name_gets_a_new_random_id_and_nothing_of_the_old()
 {
    let rig = Rig::new();
    *rig.ids.lock().unwrap() = Some(Arc::new(puddle_api::RandomIds));
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let made = |reply: &support::Reply| {
        assert_eq!(reply.status, 202, "{}", reply.body);
        let id = reply.json()["id"].as_str().unwrap().to_owned();
        assert_eq!(id.len(), 8, "{id}");
        assert_ne!(id, "acme");
        id
    };
    let first = made(&api.post("/api/workspaces", &new_workspace("acme")).await);
    events.until(ended(&first), Duration::from_secs(20)).await;
    let reply = api
        .put(
            &format!("/api/settings/workspaces/{first}"),
            &json!({"overrides": {"direct_ssh": true}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let _editor = editor_stand_in(&rig, &first).await;
    let origin = browser_origin(&rig, &api, &first).await;
    assert!(origin.contains(&format!("//{first}.localhost")), "{origin}");
    api.post(&format!("/api/workspaces/{first}/stop"), "").await;
    events.until(ended(&first), Duration::from_secs(20)).await;
    let gone = api
        .delete(
            &format!("/api/workspaces/{first}"),
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    events.until(ended(&first), Duration::from_secs(20)).await;

    let second = made(&api.post("/api/workspaces", &new_workspace("acme")).await);
    events.until(ended(&second), Duration::from_secs(20)).await;
    assert_ne!(second, first);
    let book: serde_json::Value = serde_json::from_slice(
        &std::fs::read(HostPaths::new(rig.dir.path().join("data")).workspace_book()).unwrap(),
    )
    .unwrap();
    assert_eq!(book["retired_ids"], json!([first]), "{book}");
    assert_eq!(book["workspaces"][0]["id"], second.as_str(), "{book}");
    assert_eq!(book["workspaces"][0]["name"], "acme", "{book}");
    // Nothing of the first: its settings, its origin.
    let view = api.get(&format!("/api/workspaces/{second}")).await.json();
    assert_eq!(view["direct_ssh"], false, "{view}");
    assert_eq!(view["name"], "acme", "{view}");
    let _editor = editor_stand_in(&rig, &second).await;
    let again = browser_origin(&rig, &api, &second).await;
    assert_ne!(again, origin);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sandbox_record_the_list_does_not_name_is_removed_and_reported_but_its_volume_stays() {
    let rig = Rig::new();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("lost").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let ghost = SandboxName::new("ghost").unwrap();
    rig.runtime
        .create(puddle_compute::SandboxSpec::new(
            ghost.clone(),
            ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
        ))
        .await
        .unwrap();

    let host = rig.start().await;
    assert_eq!(host.reconcile_report().removed, [ghost]);
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert_eq!(volume_names(&rig).await, ["ws-lost"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_is_refused_when_the_runtime_cannot_list_its_disks_or_sandboxes() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    for op in [Op::ListVolumes, Op::List] {
        rig.runtime.inject(
            op,
            Fault::once(puddle_compute::ComputeError::Runtime {
                op: "list",
                message: "listing broke".into(),
            }),
        );
        let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
        assert_eq!(reply.status, 503, "{}", reply.body);
        let message = reply.json()["message"].as_str().unwrap().to_owned();
        assert!(message.contains("cannot pick an id"), "{message}");
        assert!(message.contains("listing broke"), "{message}");
        assert!(
            api.get("/api/workspaces").await.json()["workspaces"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
    host.shutdown().await;
}
