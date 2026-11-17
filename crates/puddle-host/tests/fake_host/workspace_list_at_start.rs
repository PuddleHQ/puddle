// SPDX-License-Identifier: AGPL-3.0-or-later
//! Session users, owners, ids and the saved workspace list at start.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspace_runs_as_agent_and_a_switch_to_root_applies_at_the_next_start() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let got = api.get("/api/workspaces/acme").await.json();
    assert_eq!(got["session_user"], "agent", "{got}");

    // The hook adds the user, Puddle's per-user files are in its home, the volume is given to it
    // (as root) and the clone runs as it.
    let plan = String::from_utf8(rig.guest.plan_bytes(0)).unwrap();
    assert!(
        plan.contains("\npuddle_session_user 'agent' '/home/agent'\n"),
        "{plan}"
    );
    assert!(
        plan.contains("/home/agent/.gradle/init.d/puddle-proxy.gradle"),
        "{plan}"
    );
    assert!(!plan.contains("/root/.gradle"), "{plan}");
    assert_eq!(rig.guest.users_of("layout"), ["root"]);
    assert_eq!(rig.guest.users_of("clone"), ["agent"]);

    // Switched to root while it runs: saved at once, and the running guest keeps agent.
    let put = api
        .put(
            "/api/workspaces/acme/session-user",
            &json!({"session_user": "root"}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(put.json()["session_user"], "root");
    let refused = api
        .put(
            "/api/workspaces/acme/session-user",
            &json!({"session_user": "vscode"}).to_string(),
        )
        .await;
    assert_eq!(refused.status, 422, "{}", refused.body);
    let saved: serde_json::Value = serde_json::from_slice(
        &std::fs::read(rig.dir.path().join("data/workspaces.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(saved["workspaces"][0]["session_user"], "root", "{saved}");

    // The next start boots as root: no user line, root's home, the volume back to root.
    let stop = api.post("/api/workspaces/acme/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let start = api.post("/api/workspaces/acme/start", "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let plan = String::from_utf8(rig.guest.plan_bytes(rig.guest.boots() - 1)).unwrap();
    assert!(!plan.contains("puddle_session_user"), "{plan}");
    assert!(
        plan.contains("/root/.gradle/init.d/puddle-proxy.gradle"),
        "{plan}"
    );
    assert_eq!(rig.guest.users_of("layout"), ["root", "root"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_saved_before_session_users_keeps_root() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let book = json!({"version": 2, "workspaces": [{
        "id": "old", "name": "old", "repositories": [], "start_folder": null,
        "image": FakeRuntime::DEBIAN, "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024
    }]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("old").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let host = rig.start().await;
    let got = api(&host).get("/api/workspaces/old").await.json();
    assert_eq!(got["session_user"], "root", "{got}");
    host.shutdown().await;
}

/// A list from before owners (version 3) gives each workspace the user the database names, else the
/// install's owner. From then on the list is where the owner lives: every start records the owner
/// each listed workspace names, so one added to the list by hand later is its list owner's.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_records_the_owner_each_listed_workspace_names() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let entry = |id: &str| {
        json!({
            "id": id, "name": id, "repositories": [], "start_folder": null,
            "image": FakeRuntime::DEBIAN, "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024
        })
    };
    for id in ["old", "later"] {
        rig.runtime
            .create_volume(puddle_compute::VolumeSpec {
                name: WorkspaceId::new(id).unwrap().volume_name(),
                size: puddle_compute::DiskSize::mib(1024),
            })
            .await
            .unwrap();
    }
    let book = json!({"version": 3, "workspaces": [entry("old")]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    let host = rig.start().await;
    assert_eq!(
        host.store()
            .workspace_owner(&WorkspaceId::new("old").unwrap())
            .unwrap(),
        Some(puddle_store::UserId::OWNER)
    );
    // The host took the reach across every user before it served anything, so nothing the API
    // is handed can take it after.
    assert!(host.store().take_all_users().is_none());
    let ann = host
        .store()
        .add_user(&puddle_store::UserName::new("ann").unwrap())
        .unwrap()
        .id;
    host.shutdown().await;
    drop(host);

    // The list is version 4 from here: the owner is on the entry, and the list's word is kept.
    let with_owner = |id: &str, owner: i64| {
        let mut e = entry(id);
        e["owner"] = json!(owner);
        e
    };
    let book = json!({"version": 4, "workspaces": [
        with_owner("old", puddle_store::UserId::OWNER.get()),
        with_owner("later", ann.get()),
    ]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    let host = rig.start().await;
    let owner_of = |id: &str| {
        host.store()
            .workspace_owner(&WorkspaceId::new(id).unwrap())
            .unwrap()
    };
    assert_eq!(owner_of("later"), Some(ann));
    assert_eq!(owner_of("old"), Some(puddle_store::UserId::OWNER));
    host.shutdown().await;
}

/// A list and a database that disagree about whose a workspace is stop the start, saying so,
/// rather than guess; the list put right, the next start goes through.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_where_the_list_and_the_database_disagree_on_an_owner_stops() {
    let rig = Rig::new();
    let host = rig.start().await;
    let ann = host
        .store()
        .add_user(&puddle_store::UserName::new("ann").unwrap())
        .unwrap()
        .id;
    host.store()
        .set_workspace_owner(&WorkspaceId::new("old").unwrap(), ann)
        .unwrap();
    host.shutdown().await;
    drop(host);
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("old").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    let book = |owner: i64| {
        json!({"version": 4, "workspaces": [{
            "owner": owner,
            "id": "old", "name": "old", "repositories": [], "start_folder": null,
            "image": FakeRuntime::DEBIAN, "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024
        }]})
    };
    let path = rig.dir.path().join("data").join("workspaces.json");
    std::fs::write(&path, book(puddle_store::UserId::OWNER.get()).to_string()).unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("whose workspace old is could not be recorded"),
        "{err}"
    );
    std::fs::write(&path, book(ann.get()).to_string()).unwrap();
    let host = rig.start().await;
    assert_eq!(
        host.store()
            .workspace_owner(&WorkspaceId::new("old").unwrap())
            .unwrap(),
        Some(ann)
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_whose_id_differs_from_its_name_is_found_everywhere_by_the_id() {
    const ID: &str = "k7m2x9qa";
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(data.join("front")).unwrap();
    // Built by hand: a create still derives the id from the name.
    let book = json!({"version": 3, "workspaces": [{
        "id": ID, "name": "Shop API \u{fc}", "repositories": [],
        "start_folder": null, "image": FakeRuntime::DEBIAN, "memory_mib": 1024,
        "created_at": 1, "disk_size_mib": 1024
    }]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    let id = WorkspaceId::new(ID).unwrap();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: id.volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    // Leftovers of the editor under the id (to be deleted) and under the name (never touched).
    std::fs::write(data.join("front").join(format!("{ID}.key")), b"half").unwrap();
    std::fs::write(data.join("front").join("shop.key"), b"other").unwrap();

    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let got = api.get(&format!("/api/workspaces/{ID}")).await;
    assert_eq!(got.status, 200, "{}", got.body);
    assert_eq!(api.get("/api/workspaces/shop").await.status, 404);

    // The settings file: written under the id.
    allow_direct_ssh(&api, ID, true).await;
    let settings = data.join("settings").join("workspaces");
    assert!(settings.join(format!("{ID}.json")).is_file());
    assert!(!settings.join("shop.json").exists());

    let start = api.post(&format!("/api/workspaces/{ID}/start"), "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    events
        .until(ended("k7m2x9qa"), Duration::from_secs(20))
        .await;
    assert_eq!(
        status_of(&api.get(&format!("/api/workspaces/{ID}")).await),
        "running"
    );

    // The sandbox and the volume are named by the id.
    let calls = rig.runtime.calls();
    let created = calls.iter().find(|c| matches!(c.op, Op::Create)).unwrap();
    assert_eq!(created.target.as_deref(), Some(id.sandbox_name().as_str()));
    assert!(
        calls
            .iter()
            .filter_map(|c| c.target.as_deref())
            .all(|t| !t.contains("shop")),
        "{calls:?}"
    );
    let volumes: Vec<String> = rig
        .runtime
        .list_volumes()
        .await
        .unwrap()
        .iter()
        .map(|v| v.name.clone())
        .collect();
    assert_eq!(volumes, [id.volume_name().to_string()]);

    // The ssh alias is the id's.
    let ssh = std::fs::read_to_string(rig.puddle_ssh_config()).unwrap();
    assert!(ssh.contains(&format!("Host puddle-{ID}")), "{ssh}");
    assert!(!ssh.contains("shop"), "{ssh}");

    // The editor window's origin is the id's, and the window is told what to call the workspace.
    let browser = api
        .post(
            &format!("/api/workspaces/{ID}/attach"),
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(browser.json()["opened"], true, "{}", browser.body);
    let origin = rig.launcher.browser.lock().unwrap()[0].origin.clone();
    assert!(
        origin.starts_with(&format!("http://{ID}.localhost:")),
        "{origin}"
    );

    // Delete: the browser data, the key half and the problem key are the id's.
    *rig.launcher.forget_fails_with.lock().unwrap() = Some("held open".to_owned());
    stop_and_wait(&api, &mut events, "k7m2x9qa").await;
    delete_and_wait(&api, &mut events, "k7m2x9qa").await;
    assert_eq!(*rig.launcher.forgotten.lock().unwrap(), [ID]);
    assert!(!data.join("front").join(format!("{ID}.key")).exists());
    assert!(data.join("front").join("shop.key").exists());
    assert!(!settings.join(format!("{ID}.json")).exists());
    assert!(has_problem(&host, &format!("browser-data:{ID}")));
    assert!(
        host.problems()
            .list()
            .iter()
            .all(|p| !p.key.contains("shop")),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupted_create_is_dropped_and_its_volume_removed() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let book = json!({"version": 1, "workspaces": [{
        "id": "half", "name": "half", "repo_url": REPO, "image": FakeRuntime::DEBIAN,
        "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024,
        "first_connect_notice_due": true, "creating": true
    }]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("half").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();

    let host = rig.start().await;
    assert!(
        api(&host).get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(host.reconcile_report().volumes_removed.len(), 1);
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    // The book no longer lists it.
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("workspaces.json")).unwrap()).unwrap();
    assert!(
        saved["workspaces"].as_array().unwrap().is_empty(),
        "{saved}"
    );
    // Its id is retired: whatever it left keyed by it is never met again.
    assert_eq!(saved["retired_ids"], json!(["half"]), "{saved}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_damaged_workspace_list_stops_the_start_instead_of_looking_empty() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("workspaces.json"), b"{ not json").unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::State { .. }), "{err}");
    // Nothing was reconciled: a volume of the workspace that could not be listed is safe.
    assert!(rig.runtime.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_open_data_folder_is_made_owner_only_at_start_and_one_that_cannot_be_refuses() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("puddle.db"), b"").unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let host = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap();
    let db = std::fs::File::open(data.join("puddle.db")).unwrap();
    puddle_fs::private::check(&db).unwrap();
    host.shutdown().await;

    // A folder in the database's place can't be made owner-only: the start is refused before
    // anything is reconciled.
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(data.join("puddle.db")).unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(
        err.to_string()
            .contains("will not run with wider permissions"),
        "{err}"
    );
    assert!(rig.runtime.calls().is_empty());
}
