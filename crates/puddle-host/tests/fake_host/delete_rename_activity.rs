// SPDX-License-Identifier: AGPL-3.0-or-later
//! Delete, rename and the activity log.

use super::*;

/// Creates `acme` with a secret of its own and a settings file, then stops Puddle: what a
/// delete has to clear besides the runtime. Returns the secrets that exist for it.
async fn acme_with_leftovers(rig: &Rig) -> usize {
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    put_env(
        &api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(Some(REAL), &["api.example.org"]),
    )
    .await;
    allow_direct_ssh(&api, "acme", true).await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let secrets = rig.secrets.ids().len();
    assert_eq!(secrets, 1);
    host.shutdown().await;
    drop(host);
    secrets
}

fn acme_settings_file(rig: &Rig) -> PathBuf {
    rig.dir
        .path()
        .join("data")
        .join("settings")
        .join("workspaces")
        .join("acme.json")
}

fn read_list(rig: &Rig) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(list_path(rig)).unwrap()).unwrap()
}

fn mark_acme_as_being_deleted(rig: &Rig) {
    let mut book = read_list(rig);
    book["workspaces"][0]["deleting"] = json!(true);
    std::fs::write(list_path(rig), book.to_string()).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_delete_leaves_the_list_without_the_entry_and_with_the_id_retired() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    delete_and_wait(&api, &mut events, "acme").await;
    let book = read_list(&rig);
    assert_eq!(book["workspaces"], json!([]), "{book}");
    assert_eq!(book["retired_ids"], json!(["acme"]), "{book}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_that_went_as_far_as_the_volume_is_finished_at_the_next_start() {
    let rig = Rig::named_ids();
    acme_with_leftovers(&rig).await;
    // Puddle died after the volume went, before the cleanup: the entry is still marked.
    mark_acme_as_being_deleted(&rig);
    rig.runtime
        .remove_volume(&name("acme").volume_name())
        .await
        .unwrap();
    assert!(acme_settings_file(&rig).is_file());
    assert!(
        !rig.runtime.list().await.unwrap().is_empty(),
        "the sandbox is still there"
    );

    let host = rig.start().await;
    let api = api(&host);
    assert_eq!(api.get("/api/workspaces/acme").await.status, 404);
    // The sweep runs in the background: wait for its last step, then check all of them.
    eventually("the browser data of acme forgotten", APPLY, || {
        rig.launcher
            .forgotten
            .lock()
            .unwrap()
            .contains(&"acme".to_owned())
            .then_some(())
    })
    .await;
    assert!(rig.secrets.ids().is_empty(), "the secret of acme is left");
    assert!(!acme_settings_file(&rig).exists());
    assert!(
        rig.runtime.list().await.unwrap().is_empty(),
        "the sandbox of acme is left"
    );
    let book = read_list(&rig);
    assert_eq!(book["workspaces"], json!([]), "{book}");
    assert_eq!(book["retired_ids"], json!(["acme"]), "{book}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_that_never_reached_the_volume_leaves_the_workspace_as_it_was() {
    let rig = Rig::named_ids();
    acme_with_leftovers(&rig).await;
    mark_acme_as_being_deleted(&rig);

    let host = rig.start().await;
    let api = api(&host);
    let got = api.get("/api/workspaces/acme").await.json();
    assert_eq!(got["status"], "stopped", "{got}");
    let book = read_list(&rig);
    assert!(book["workspaces"][0].get("deleting").is_none(), "{book}");
    assert_eq!(book.get("retired_ids"), None, "{book}");
    // Nothing of it was cleared, and it starts.
    assert_eq!(rig.secrets.ids().len(), 1);
    assert!(acme_settings_file(&rig).is_file());
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retired_id_whose_cleanup_failed_is_cleaned_at_the_next_start() {
    let rig = Rig::named_ids();
    acme_with_leftovers(&rig).await;
    // An earlier Puddle deleted acme completely except for its own secret and settings file
    // (the cleanup only left notices): the list has it retired, the volume is gone.
    let mut book = read_list(&rig);
    book["workspaces"] = json!([]);
    book["retired_ids"] = json!(["acme"]);
    std::fs::write(list_path(&rig), book.to_string()).unwrap();
    rig.runtime
        .remove_volume(&name("acme").volume_name())
        .await
        .unwrap();

    let host = rig.start().await;
    eventually("the leftovers of acme gone", APPLY, || {
        (rig.secrets.ids().is_empty() && !acme_settings_file(&rig).exists()).then_some(())
    })
    .await;
    assert_eq!(read_list(&rig)["retired_ids"], json!(["acme"]));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sandbox_of_a_deleted_workspace_that_cannot_be_removed_is_a_problem_and_the_rest_is_still_cleaned()
 {
    let rig = Rig::named_ids();
    acme_with_leftovers(&rig).await;
    let mut book = read_list(&rig);
    book["workspaces"] = json!([]);
    book["retired_ids"] = json!(["acme"]);
    std::fs::write(list_path(&rig), book.to_string()).unwrap();
    rig.runtime
        .remove_volume(&name("acme").volume_name())
        .await
        .unwrap();
    rig.runtime.inject(
        Op::Remove,
        Fault::always(disk_error("remove")).only_for("acme"),
    );

    let host = rig.start().await;
    let problem = wait_for_problem(&host, "left-sandbox:acme").await;
    assert!(problem.title.contains("acme"), "{problem:?}");
    assert!(problem.detail.contains("no space left"), "{problem:?}");
    // The sandbox stays, and the other leftovers are cleared all the same.
    eventually("the leftovers of acme gone", APPLY, || {
        (rig.secrets.ids().is_empty() && !acme_settings_file(&rig).exists()).then_some(())
    })
    .await;
    assert_eq!(rig.runtime.list().await.unwrap().len(), 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_workspace_list_keeps_every_workspace_volume() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    drop(events);
    // The list is gone (deleted by hand, restored from an old backup, a broken disk).
    std::fs::remove_file(list_path(&rig)).unwrap();

    let again = rig.start().await;
    assert_eq!(volume_names(&rig).await, ["ws-acme"]);
    assert!(
        again.reconcile_report().volumes_removed.is_empty(),
        "{:?}",
        again.reconcile_report()
    );
    // Reported, so the user can be told, and still usable: the volume is never taken.
    assert_eq!(
        again.reconcile_report().unknown_volumes,
        [WorkspaceId::new("acme").unwrap().volume_name()]
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_volume_no_listed_workspace_claims_is_kept() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    drop(events);
    // A volume with work on it that the list doesn't name (an older or partial list).
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("lost").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();

    let again = rig.start().await;
    assert_eq!(volume_names(&rig).await, ["ws-acme", "ws-lost"]);
    assert_eq!(
        again.reconcile_report().unknown_volumes,
        [WorkspaceId::new("lost").unwrap().volume_name()]
    );
    // The listed workspace is unaffected.
    assert_eq!(
        status_of(&api(&again).get("/api/workspaces/acme").await),
        "stopped"
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_is_refused_when_the_workspace_list_cannot_be_saved() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    make_unwritable(&list_path(&rig));

    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    let message = reply.json()["message"].as_str().unwrap().to_owned();
    // What failed, the file, and what to do about it.
    assert!(message.contains("workspace list"), "{message}");
    assert!(message.contains("workspaces.json"), "{message}");
    assert!(message.contains("try again"), "{message}");
    // Nothing was made, and the name is still free.
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
async fn a_create_whose_last_save_fails_is_undone_and_says_why() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    *rig.guest.break_list_on_clone.lock().unwrap() = Some(list_path(&rig));

    api1.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    // The list still says "being created", so a restart would take the volume as a leftover:
    // the create fails now, before anyone works in it.
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("workspace list"),
        "{end}"
    );
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(volume_names(&rig).await.is_empty());
    host.shutdown().await;
}

fn name_body(shown: &str) -> String {
    json!({"name": shown}).to_string()
}

/// A save of the workspace list writes each name into the store's database. The store has one
/// connection that proxy decisions and audit searches share, so a busy database must not hold up
/// every reader of the workspace list: no write to it happens under the list's lock.
#[tokio::test(flavor = "multi_thread")]
async fn a_busy_database_does_not_hold_up_the_workspace_list_while_a_rename_saves() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    after_boot_listing(&rig).await;
    // Another connection holds the write lock: the name write waits for it (up to 5 s).
    let db = rusqlite::Connection::open(rig.dir.path().join("data").join("puddle.db")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    let body = name_body("Acme shop");
    let rename = api.patch("/api/workspaces/acme", &body);
    let id = WorkspaceId::new("acme").unwrap();
    let probe = async {
        // flake-ok: nothing signals that the rename has reached its write to the database; a late probe only makes the test pass where the old code would fail
        tokio::time::sleep(Duration::from_millis(500)).await;
        // The list's lock is a plain mutex, so a reader that has to wait blocks its thread:
        // measure how long the read took rather than time it out.
        let started = std::time::Instant::now();
        let read = host.workspaces().get(&id).await;
        let took = started.elapsed();
        db.execute_batch("COMMIT").unwrap();
        (read, took)
    };
    let (renamed, (read, took)) = tokio::join!(rename, probe);
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    assert!(read.is_ok());
    // flake-ok: the bar is 4 s against the 5 s the database makes a writer wait; a read of the list takes microseconds
    assert!(
        took < Duration::from_secs(4),
        "the workspace list waited {took:?} for a write to the database"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_survives_a_restart_and_leaves_the_sandbox_and_volume_alone() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    // The create ends before its background listing runs; count after it, or it lands in the rename.
    after_boot_listing(&rig).await;
    let calls_before = rig.runtime.calls().len();
    let renamed = api1
        .patch("/api/workspaces/acme", &name_body("Acme shop"))
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    let ws = renamed.json();
    assert_eq!(
        (&ws["id"], &ws["name"], &ws["status"]),
        (&json!("acme"), &json!("Acme shop"), &json!("running"))
    );
    // The workspace is running and nothing in the runtime was touched: a name only.
    assert_eq!(rig.runtime.calls().len(), calls_before);
    assert_eq!(volume_names(&rig).await, ["ws-acme"]);
    host.shutdown().await;
    drop(host);
    drop(events);

    let again = rig.start().await;
    let api2 = api(&again);
    let ws = api2.get("/api/workspaces/acme").await.json();
    assert_eq!(
        (&ws["id"], &ws["name"]),
        (&json!("acme"), &json!("Acme shop"))
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(list_path(&rig)).unwrap()).unwrap();
    assert_eq!(saved["version"], 3);
    assert_eq!(saved["workspaces"][0]["name"], "Acme shop");
    assert!(
        saved["workspaces"][0].get("display_name").is_none(),
        "{saved}"
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_workspaces_never_share_a_label_and_a_create_cannot_take_one_in_use() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "docs").await;
    assert_eq!(
        api.patch("/api/workspaces/acme", &name_body("Handbook"))
            .await
            .status,
        200
    );
    let clash = api
        .patch("/api/workspaces/docs", &name_body("handbook"))
        .await;
    assert_eq!(clash.status, 409, "{}", clash.body);
    let taken = api
        .post("/api/workspaces", &new_workspace("handbook"))
        .await;
    assert_eq!(taken.status, 409, "{}", taken.body);
    assert_eq!(volume_names(&rig).await, ["ws-acme", "ws-docs"]);
    // Another workspace's name is not free for this one.
    assert_eq!(
        api.patch("/api/workspaces/acme", &name_body("DOCS"))
            .await
            .status,
        409
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_allowed_while_a_start_runs() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let started = api.post("/api/workspaces/acme/start", "").await;
    assert_eq!(started.status, 202, "{}", started.body);
    // The start is in flight: only the label changes, so the rename goes through.
    let during = api.patch("/api/workspaces/acme", &name_body("Acme")).await;
    assert_eq!(during.status, 200, "{}", during.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    let ws = api.get("/api/workspaces/acme").await.json();
    assert_eq!(
        (&ws["status"], &ws["name"]),
        (&json!("running"), &json!("Acme"))
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_refused_once_shutdown_has_begun() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    host.workspaces().close();
    let refused = api
        .patch("/api/workspaces/acme", &name_body("Acme shop"))
        .await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.body.contains("shutting down"), "{}", refused.body);
    assert_eq!(api.get("/api/workspaces/acme").await.json()["name"], "acme");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_undone_when_the_workspace_list_cannot_be_saved() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    make_unwritable(&list_path(&rig));
    let refused = api
        .patch("/api/workspaces/acme", &name_body("Acme shop"))
        .await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    let message = refused.json()["message"].as_str().unwrap().to_owned();
    assert!(
        message.contains("not renamed") && message.contains("workspace list"),
        "{message}"
    );
    // The user is not told a name that a restart would undo.
    assert_eq!(api.get("/api/workspaces/acme").await.json()["name"], "acme");
    host.shutdown().await;
}

fn op_summary(record: &serde_json::Value) -> (String, String, String) {
    let text = |key: &str| record[key].as_str().unwrap_or("").to_owned();
    (text("op"), text("workspace_id"), text("outcome"))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_created_and_a_deleted_workspace_are_in_the_activity_log_with_who_asked() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let accepted = api
        .delete("/api/workspaces/acme", Some(r#"{"confirm": true}"#))
        .await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let ops = workspace_ops("workspace=acme", &api).await;
    let seen: Vec<_> = ops.iter().map(op_summary).collect();
    assert_eq!(
        seen,
        [
            ("create".into(), "acme".into(), "succeeded".into()),
            ("delete".into(), "acme".into(), "succeeded".into()),
        ]
    );
    assert_eq!(ops[0]["actor"], "api", "{}", ops[0]);
    assert!(ops[1]["detail"].is_null(), "{}", ops[1]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_whose_volume_was_already_gone_says_so_in_the_activity_log() {
    let (_rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    let check = api.get("/api/workspaces/acme/delete-check").await.json();
    let seen = json!({"confirm": true, "fingerprint": check["fingerprint"]}).to_string();
    api.delete("/api/workspaces/acme", Some(&seen)).await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // The first record is the create that seeded the workspace.
    let ops = workspace_ops("outcome=succeeded", &api).await;
    assert_eq!(ops.len(), 2, "{ops:?}");
    assert_eq!(op_summary(&ops[1]).0, "delete");
    assert_eq!(ops[1]["detail"], "deleted; its volume was already gone");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_create_start_and_stop_are_failures_in_the_activity_log_that_the_filter_finds() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    // A create that fails.
    let parent = shell_init_folder(&rig, "acme")
        .parent()
        .unwrap()
        .to_path_buf();
    std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
    let _ = std::fs::remove_dir_all(&parent);
    std::fs::write(&parent, "in the way").unwrap();
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    std::fs::remove_file(&parent).unwrap();
    // A stop that fails, on a workspace of the same name that then runs (under another id: the
    // failed create's is retired).
    create_as(&api, &mut events, "acme", "acme-2").await;
    rig.runtime
        .inject(Op::Stop, Fault::once(disk_error("stop")).only_for("acme-2"));
    api.post("/api/workspaces/acme-2/stop", "").await;
    events.until(ended("acme-2"), Duration::from_secs(20)).await;

    let failed = workspace_ops("outcome=failed", &api).await;
    let seen: Vec<_> = failed.iter().map(op_summary).collect();
    assert_eq!(
        seen,
        [
            ("create".into(), "acme".into(), "failed".into()),
            ("stop".into(), "acme-2".into(), "failed".into()),
        ]
    );
    for record in &failed {
        assert!(
            record["detail"].as_str().is_some_and(|d| !d.is_empty()),
            "a failure says why: {record}"
        );
    }
    // The outcome filter splits the one successful create from the two failures.
    let succeeded = workspace_ops("outcome=succeeded", &api).await;
    assert_eq!(succeeded.len(), 1, "{succeeded:?}");
    assert_eq!(op_summary(&succeeded[0]).0, "create");
    // Another workspace's filter finds none of them.
    assert!(workspace_ops("workspace=other", &api).await.is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_start_is_in_the_activity_log_and_a_start_that_worked_is_not() {
    let (_rig, host) = host_with_listed_workspace_whose_volume_is_gone().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // The first record is the create that seeded the workspace.
    let ops = workspace_ops("", &api).await;
    assert_eq!(ops.len(), 2, "{ops:?}");
    assert_eq!(
        op_summary(&ops[1]),
        ("start".into(), "acme".into(), "failed".into())
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_started_and_stopped_workspace_leaves_no_start_or_stop_records() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let seen: Vec<_> = workspace_ops("", &api)
        .await
        .iter()
        .map(op_summary)
        .collect();
    assert_eq!(seen, [("create".into(), "acme".into(), "succeeded".into())]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rename_is_in_the_activity_log_and_so_is_one_that_could_not_be_saved() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let renamed = api
        .patch("/api/workspaces/acme", &name_body("Acme shop"))
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    make_unwritable(&list_path(&rig));
    let refused = api
        .patch("/api/workspaces/acme", &name_body("Acme store"))
        .await;
    assert_eq!(refused.status, 503, "{}", refused.body);

    let ops = workspace_ops("workspace=acme", &api).await;
    let seen: Vec<_> = ops.iter().map(op_summary).collect();
    assert_eq!(
        seen,
        [
            ("create".into(), "acme".into(), "succeeded".into()),
            ("rename".into(), "acme".into(), "succeeded".into()),
            ("rename".into(), "acme".into(), "failed".into()),
        ]
    );
    assert_eq!(ops[1]["detail"], "renamed to Acme shop");
    assert!(
        ops[2]["detail"].as_str().unwrap().contains("not renamed"),
        "{}",
        ops[2]
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_version_2_list_of_the_previous_release_starts_with_the_display_name_as_the_name() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let path = data.join("workspaces.json");
    let old = include_str!("../fixtures/workspaces-v2.json");
    std::fs::write(&path, old).unwrap();
    let host = rig.start().await;
    let api = api(&host);
    let list = api.get("/api/workspaces").await.json();
    let shown: Vec<(&str, &str)> = list["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["id"].as_str().unwrap(), w["name"].as_str().unwrap()))
        .collect();
    assert_eq!(shown, [("shop", "Shop API \u{fc}"), ("tools", "tools")]);
    assert!(
        list["workspaces"][0].get("display_name").is_none(),
        "{list}"
    );
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        old,
        "a start never writes the list"
    );
    // Renaming saves version 3; the id and everything keyed by it stay.
    let renamed = api
        .patch("/api/workspaces/shop", &name_body("Web shop"))
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["version"], 3);
    assert_eq!(
        (
            saved["workspaces"][0]["id"].as_str(),
            saved["workspaces"][0]["name"].as_str()
        ),
        (Some("shop"), Some("Web shop"))
    );
    assert_eq!(saved["workspaces"][1]["name"], "tools");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_activity_record_keeps_the_name_the_workspace_had_when_it_was_written() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let renamed = api
        .patch("/api/workspaces/acme", &name_body("Acme shop"))
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    let page = api.get("/api/audit?type=workspace_op&after=0").await.json();
    let rows: Vec<(String, String)> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["record"]["op"].as_str().unwrap().to_owned(),
                e["workspace_name"].as_str().unwrap_or("<none>").to_owned(),
            )
        })
        .collect();
    // The create was written under the name it had; the rename, under the new one.
    assert_eq!(rows.first().map(|r| r.1.as_str()), Some("acme"), "{rows:?}");
    assert_eq!(
        rows.last().map(|r| r.1.as_str()),
        Some("Acme shop"),
        "{rows:?}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_list_with_a_bad_name_stops_the_start() {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let book = json!({"version": 1, "workspaces": [{
        "id": "acme", "name": "two\nlines", "display_name": "two\nlines", "repo_url": REPO,
        "image": FakeRuntime::DEBIAN, "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024
    }]});
    std::fs::write(data.join("workspaces.json"), book.to_string()).unwrap();
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::State { .. }), "{err}");
    assert!(err.to_string().contains("bad name"), "{err}");
    // The workspace's volume is safe: nothing was reconciled.
    assert!(rig.runtime.calls().is_empty());
}

/// The delete check boots an image of its own, and the first pull of that image takes longer than
/// a request may: it is downloaded when a workspace stops, not when the delete asks for it.
#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_workspace_downloads_the_image_the_delete_check_boots() {
    const CHECK_IMAGE: &str = puddle_workspace::WorkspaceConfig::DEFAULT_MAINTENANCE_IMAGE;
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let pulls = || {
        rig.runtime
            .calls()
            .iter()
            .filter(|c| c.op == Op::PullImage && c.target.as_deref() == Some(CHECK_IMAGE))
            .count()
    };
    create(&api, &mut events, "acme").await;
    // The create pulled the workspace's own image, which here is the same one.
    let before = pulls();
    // The download fails here: the check and the delete go on, and say why themselves.
    rig.runtime.inject(
        Op::PullImage,
        Fault::once(puddle_compute::ComputeError::ImagePull {
            image: CHECK_IMAGE.into(),
            reason: "offline".into(),
        })
        .only_for(CHECK_IMAGE),
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    eventually(
        "the check image is requested",
        Duration::from_secs(5),
        || pulls() > before,
    )
    .await;
    assert_eq!(pulls(), before + 1, "{:?}", rig.runtime.calls());
    let check = api.get("/api/workspaces/acme/delete-check").await;
    assert_eq!(check.status, 200, "{}", check.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_that_fails_and_cannot_save_the_list_says_both() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    rig.runtime
        .inject(Op::RemoveVolume, Fault::always(disk_error("remove volume")));
    // The delete's mark is saved; its failure then finds the list unwritable.
    *rig.launcher.unwritable_on_close.lock().unwrap() = Some(list_path(&rig));

    let confirm = json!({"confirm": true}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&confirm)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("no space left"), "the cause: {detail}");
    assert!(detail.contains("workspace list"), "the save: {detail}");
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "stopped");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_is_refused_when_the_workspace_list_cannot_be_saved() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    make_unwritable(&list_path(&rig));

    let confirm = json!({"confirm": true}).to_string();
    let refused = api.delete("/api/workspaces/acme", Some(&confirm)).await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    let message = refused.json()["message"].as_str().unwrap().to_owned();
    assert!(message.contains("workspace list"), "{message}");
    // Nothing was deleted: the workspace and its volume are still there.
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "stopped");
    assert_eq!(volume_names(&rig).await, ["ws-acme"]);
    // Not busy any more: once the list can be saved again, the delete goes through.
    std::fs::remove_file(list_path(&rig).join("keep")).unwrap();
    std::fs::remove_dir(list_path(&rig)).unwrap();
    let accepted = api.delete("/api/workspaces/acme", Some(&confirm)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(volume_names(&rig).await.is_empty());
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(list_path(&rig)).unwrap()).unwrap();
    assert!(
        saved["workspaces"].as_array().unwrap().is_empty(),
        "{saved}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ssh_endpoint_exists_while_the_workspace_runs_with_direct_ssh_on() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    let name = WorkspaceId::new("acme").unwrap();
    let endpoint: PathBuf = host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_ne!(endpoint.as_os_str(), "");
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(host.workspaces().ssh_endpoint(&name).await.is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspace_does_not_inherit_the_settings_of_a_deleted_one_of_the_same_name() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    let name = WorkspaceId::new("acme").unwrap();
    assert!(host.workspaces().direct_ssh_allowed(&name));
    let file = rig
        .dir
        .path()
        .join("data")
        .join("settings")
        .join("workspaces")
        .join("acme.json");
    assert!(file.is_file());
    stop_and_wait(&api, &mut events, "acme").await;
    delete_and_wait(&api, &mut events, "acme").await;
    assert!(!file.exists(), "a delete removes the settings file");
    create_as(&api, &mut events, "acme", "acme-2").await;
    let view = api.get("/api/workspaces/acme-2").await.json();
    assert_eq!(view["direct_ssh"], false, "{view}");
    assert!(
        !host
            .workspaces()
            .direct_ssh_allowed(&WorkspaceId::new("acme-2").unwrap())
    );
    assert!(!host.workspaces().direct_ssh_allowed(&name));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_that_cannot_remove_the_settings_file_says_so_and_still_deletes() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    stop_and_wait(&api, &mut events, "acme").await;
    let file = rig
        .dir
        .path()
        .join("data")
        .join("settings")
        .join("workspaces")
        .join("acme.json");
    make_unwritable(&file);
    delete_and_wait(&api, &mut events, "acme").await;
    let problem = problem_of(&host, "settings:acme");
    assert!(problem.title.contains("acme"), "{problem:?}");
    assert!(problem.detail.contains("settings"), "{problem:?}");
    let list = api.get("/api/workspaces").await;
    assert!(!list.body.contains("acme"), "{}", list.body);
    assert!(
        problem
            .detail
            .contains("No later workspace gets the id acme"),
        "{problem:?}"
    );
    // The next workspace of the name has another id and none of the old settings.
    create_as(&api, &mut events, "acme", "acme-2").await;
    let view = api.get("/api/workspaces/acme-2").await.json();
    assert_eq!(view["direct_ssh"], false, "{view}");
    host.shutdown().await;
}

/// A settings change in flight holds the settings lock from its read to its write. A delete that
/// finishes in between would let the write bring the file back for the next workspace of the
/// name, so the delete waits for it.
#[tokio::test(flavor = "multi_thread")]
async fn a_delete_waits_for_a_settings_change_in_flight_so_it_cannot_bring_the_file_back() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    stop_and_wait(&api, &mut events, "acme").await;
    let file = rig
        .dir
        .path()
        .join("data")
        .join("settings")
        .join("workspaces")
        .join("acme.json");
    let in_flight = host.settings().lock().await;
    // Straight to the service: the HTTP route reads settings under the same lock.
    let id = WorkspaceId::new("acme").unwrap();
    puddle_api::WorkspaceService::delete(host.workspaces(), &id, None)
        .await
        .unwrap();
    // Everything but the settings file goes; that waits on the change in flight.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        file.is_file(),
        "the delete took the file while a change was in flight"
    );
    drop(in_flight);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(
        !file.exists(),
        "the delete removes the file once the change is done"
    );
    host.shutdown().await;
}

/// The clean-up of deleted workspaces runs in the background at start: a credential store that
/// holds it up never holds up a quit, and an id whose leftovers are all gone is not cleaned again
/// at every later start.
#[tokio::test(flavor = "multi_thread")]
async fn the_clean_up_of_deleted_workspaces_never_holds_up_a_quit_and_is_not_repeated_once_done() {
    let rig = Rig::named_ids();
    *rig.deletes.counted.lock().unwrap() = Some(WorkspaceId::new("acme").unwrap());
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    delete_and_wait(&api, &mut events, "acme").await;
    host.shutdown().await;
    drop(host);
    assert!(
        rig.deletes.count.load(Ordering::SeqCst) > 0,
        "the delete removed no saved login"
    );
    // A start cleans acme up for good; the next start finds nothing to do.
    let host = rig.start().await;
    host.shutdown().await;
    drop(host);
    let before = rig.deletes.count.load(Ordering::SeqCst);
    let host = rig.start().await;
    host.shutdown().await;
    drop(host);
    assert_eq!(
        rig.deletes.count.load(Ordering::SeqCst),
        before,
        "a finished clean-up ran again"
    );

    // A delete that another Puddle did not finish, under a store that does not answer for it.
    let mut book = read_list(&rig);
    book["retired_ids"] = json!(["acme", "gone"]);
    std::fs::write(list_path(&rig), book.to_string()).unwrap();
    *rig.deletes.held.lock().unwrap() = Some(WorkspaceId::new("gone").unwrap());
    let host = rig.start().await;
    let quit = tokio::time::timeout(Duration::from_secs(60), host.shutdown()).await;
    *rig.deletes.held.lock().unwrap() = None;
    assert!(quit.is_ok(), "the quit waited for the held clean-up");
    drop(host);
}
