// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cloning, adding, managing and listing a workspace's repositories.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_sync_after_the_clones_leaves_nothing_behind() {
    let rig = Rig::named_ids();
    rig.guest.fail_sync.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed");
    assert!(
        end["detail"].as_str().unwrap().contains("sync: I/O error"),
        "{end}"
    );
    assert_eq!(
        events.steps("acme"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "syncing",
            "cloning",
            "failed"
        ]
    );
    // No record, no sandbox, no volume, no VM.
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    // The name is free again; the failed create's id is retired with what may be keyed by it.
    rig.guest.fail_sync.store(false, Ordering::SeqCst);
    create_as(&api, &mut events, "acme", "acme-2").await;
    host.shutdown().await;
}

/// A list of repositories without its generated entry ids.
fn without_entries(list: &serde_json::Value) -> serde_json::Value {
    let mut list = list.clone();
    for r in list.as_array_mut().unwrap() {
        r.as_object_mut().unwrap().remove("entry");
    }
    list
}

/// The only problem standing is the failed clone of `url` into `workspace`'s `dir`, with git's
/// error first and the way out last.
async fn the_one_problem_names_the_clone(api: &Api, workspace: &str, dir: &str, url: &str) {
    let entry = entry_of(api, workspace, dir).await;
    let problems = problems_of(api).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    let (key, about, title, detail) = &problems[0];
    assert_eq!(key, &format!("clone:{workspace}:{entry}"));
    assert_eq!(about.as_deref(), Some(workspace));
    assert_eq!(
        title,
        &format!("Puddle could not clone {url} into {workspace}")
    );
    assert!(
        detail.starts_with(&format!(
            "git exited 128: fatal: unable to access '{url}': Could not resolve host"
        )),
        "{detail}"
    );
    assert!(
        detail.ends_with(&format!("git clone {url} /workspaces/{workspace}/{dir}")),
        "{detail}"
    );
}

/// Records, at each clone of `workspace`, the identities and repository table rows it has by then.
fn git_settings_at_each_clone(
    rig: &Rig,
    host: &Host<FakeRuntime>,
    workspace: &str,
) -> Arc<Mutex<Vec<String>>> {
    let store = host.store().clone();
    let workspace = WorkspaceId::new(workspace).unwrap();
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let look = seen.clone();
    *rig.guest.at_clone.lock().unwrap() = Some(Box::new(move |url: &str| {
        let git = store.workspace_git(&workspace).unwrap();
        let rows: Vec<String> = git.repos.iter().map(|r| r.repo.to_string()).collect();
        let identities: Vec<String> = git.identities.iter().map(|i| i.label.clone()).collect();
        look.lock().unwrap().push(format!(
            "clone {url}: identities {identities:?}, rows {rows:?}"
        ));
    }));
    seen
}

/// An identity "Work" that covers `example.org/acme`.
async fn make_work_identity(api: &Api) {
    let work = api
        .post(
            "/api/identities",
            &json!({
                "label": "Work",
                "author": {"name": "Work", "email": "work@example.org"},
                "credentials": [{
                    "host": "example.org",
                    "source": {"kind": "gh", "host": "example.org", "account": "me"},
                    "covers": {"owners": ["acme"], "rest_of_host": false}
                }]
            })
            .to_string(),
        )
        .await;
    assert_eq!(work.status, 201, "{}", work.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clone_that_fails_fails_only_its_repository_and_every_clone_carries_its_git_settings() {
    const DOWN: &str = "https://unreachable.example/acme/docs.git";
    let rig = Rig::named_ids();
    *rig.guest.fail_clone_of.lock().unwrap() = Some(DOWN.to_owned());
    let host = rig.start().await;
    let seen = git_settings_at_each_clone(&rig, &host, "trio");
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": "trio", "repositories": [
                {"url": REPO},
                {"url": DOWN},
                {"url": "https://example.org/acme/web", "push": false}
            ]})
            .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("trio"), Duration::from_secs(20)).await;
    assert_eq!(
        end["step"], "done",
        "a failed clone does not fail the create: {end}"
    );
    assert_eq!(
        events.steps("trio"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "syncing",
            "cloning",
            "cloning",
            "cloning",
            "done"
        ]
    );
    // The identity and every table row were in place before the first clone ran.
    let all_rows =
        r#"["example.org/acme/api", "unreachable.example/acme/docs", "example.org/acme/web"]"#;
    assert_eq!(
        *seen.lock().unwrap(),
        [
            format!(r#"clone {REPO}: identities ["Work"], rows {all_rows}"#),
            format!(r#"clone {DOWN}: identities ["Work"], rows {all_rows}"#),
            format!(r#"clone https://example.org/acme/web: identities ["Work"], rows {all_rows}"#),
        ]
    );
    assert_eq!(
        rig.guest.clone_commands(),
        [
            format!("clone -- {REPO} /workspaces/trio/api"),
            format!("clone -- {DOWN} /workspaces/trio/docs"),
            "clone -- https://example.org/acme/web /workspaces/trio/web".to_owned(),
        ]
    );
    assert_eq!(rig.guest.syncs(), 1, "one sync after the last clone");
    // The workspace runs, keeps all three entries, and a problem names what was not cloned.
    let ws = api.get("/api/workspaces/trio").await.json();
    assert_eq!(ws["status"], "running");
    assert_eq!(
        without_entries(&ws["repositories"]),
        json!([
            {"url": REPO, "directory": "api"},
            {"url": DOWN, "directory": "docs"},
            {"url": "https://example.org/acme/web", "directory": "web"}
        ])
    );
    the_one_problem_names_the_clone(&api, "trio", "docs", DOWN).await;
    let git = api.get("/api/workspaces/trio/git").await.json();
    let toggles: Vec<_> = git["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["repo"].clone(), r["push"].clone()))
        .collect();
    assert_eq!(
        toggles,
        [
            (json!("api"), json!(true)),
            (json!("docs"), json!(true)),
            (json!("web"), json!(false))
        ]
    );
    stop_and_delete(&api, &mut events, "trio").await;
    assert_eq!(
        problems_of(&api).await,
        [],
        "deleting the workspace ends its problem"
    );
    host.shutdown().await;
}

async fn stop_and_delete(api: &Api, events: &mut support::Events, name: &'static str) {
    api.post(&format!("/api/workspaces/{name}/stop"), "").await;
    events.until(ended(name), Duration::from_secs(20)).await;
    let deleted = api
        .delete(
            &format!("/api/workspaces/{name}"),
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(deleted.status, 202, "{}", deleted.body);
    events.until(ended(name), Duration::from_secs(20)).await;
}

const WEB: &str = "https://example.org/acme/web";

/// Adds `body` to workspace `name`'s list and waits for the clone it starts to end.
async fn add_and_wait(
    api: &Api,
    events: &mut support::Events,
    name: &'static str,
    body: serde_json::Value,
) -> support::Reply {
    let reply = api
        .post(
            &format!("/api/workspaces/{name}/repositories"),
            &body.to_string(),
        )
        .await;
    if reply.status == 202 {
        events.until(ended(name), Duration::from_secs(20)).await;
    }
    reply
}

#[tokio::test(flavor = "multi_thread")]
async fn an_added_clone_has_its_identity_and_table_row_before_it_runs_and_is_listed_after() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    let seen = git_settings_at_each_clone(&rig, &host, "adds");

    let reply = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": WEB}),
    )
    .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "cloning");
    assert_eq!(
        *seen.lock().unwrap(),
        [format!(
            r#"clone {WEB}: identities ["Work"], rows ["example.org/acme/api", "example.org/acme/web"]"#
        )]
    );
    assert_eq!(
        rig.guest.clone_commands().last().unwrap(),
        &format!("clone -- {WEB} /workspaces/adds/web")
    );
    assert_eq!(
        rig.guest.syncs(),
        2,
        "one sync after each of the two clones"
    );
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["busy"], serde_json::Value::Null, "{ws}");
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 2, "{ws}");

    // The listing reads the volume after the clone and ties each checkout to its entry.
    let list = checkouts_of(&api, "adds").await;
    assert_eq!(list["running"], true);
    let dirs: Vec<_> = list["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["directory"].as_str().unwrap(), c["entry"]["url"].as_str()))
        .collect();
    assert_eq!(dirs, [("api", Some(REPO)), ("web", Some(WEB))]);
    assert_eq!(list["missing"], json!([]));

    // A taken directory is refused and a listed address cannot be added twice.
    let again = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": WEB}),
    )
    .await;
    assert_eq!(again.status, 409, "{}", again.body);
    let taken = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": "https://example.org/other/web", "directory": "web"}),
    )
    .await;
    assert_eq!(taken.status, 409, "{}", taken.body);
    assert_eq!(
        rig.guest.clone_commands().len(),
        2,
        "no clone ran for either"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_added_clone_keeps_its_entry_and_one_problem_and_cloning_again_clears_it() {
    const DOWN: &str = "https://unreachable.example/acme/docs.git";
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    *rig.guest.fail_clone_of.lock().unwrap() = Some(DOWN.to_owned());

    let reply = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": DOWN}),
    )
    .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 2, "{ws}");
    the_one_problem_names_the_clone(&api, "adds", "docs", DOWN).await;
    // The entry shows as not on the disk.
    assert_eq!(checkouts_of(&api, "adds").await["missing"], json!(["docs"]));

    // Cloning again while the directory is empty works and the problem goes.
    *rig.guest.fail_clone_of.lock().unwrap() = None;
    let docs = entry_of(&api, "adds", "docs").await;
    // A directory is no entry id, so the way a client used to name the entry is a 404 now.
    let reply = api
        .post("/api/workspaces/adds/repositories/docs/clone", "")
        .await;
    assert_eq!(reply.status, 404, "{}", reply.body);
    let reply = api
        .post(
            &format!("/api/workspaces/adds/repositories/{docs}/clone"),
            "",
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("adds"), Duration::from_secs(20)).await;
    assert!(problems_of(&api).await.is_empty());
    assert_eq!(checkouts_of(&api, "adds").await["missing"], json!([]));
    // An entry that is on the disk is not cloned over.
    let reply = api
        .post(
            &format!("/api/workspaces/adds/repositories/{docs}/clone"),
            "",
        )
        .await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    let reply = api
        .post("/api/workspaces/adds/repositories/a0000000/clone", "")
        .await;
    assert_eq!(reply.status, 404, "{}", reply.body);
    host.shutdown().await;
}

const MOVED_REMOTE: &str = "https://example.org/acme/moved.git";

/// A running host with workspace `adds` and two foreign checkouts, `legacy` (https) and `ssh`.
struct ManagedRig {
    rig: Rig,
    host: Host<FakeRuntime>,
    api: Api,
    events: support::Events,
}

async fn managed_checkouts_rig() -> ManagedRig {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    rig.guest
        .put_checkout("legacy", "https://example.org/acme/legacy.git");
    rig.guest
        .put_checkout("ssh", "git@example.org:acme/ssh.git");
    ManagedRig {
        rig,
        host,
        api,
        events,
    }
}

/// The listing offers `legacy`; then its remote changes, so the user's view is stale.
async fn move_the_legacy_remote(m: &ManagedRig) {
    // A listing made at the same millisecond as the call would be shared with it.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let list = checkouts_of(&m.api, "adds").await;
    let legacy = list["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["directory"] == "legacy")
        .unwrap()
        .clone();
    assert_eq!(legacy["entry"], serde_json::Value::Null);
    assert_eq!(legacy["manage"]["offered"], true);
    m.rig.guest.put_checkout("legacy", MOVED_REMOTE);
}

async fn manage_legacy_at_its_new_remote(m: &mut ManagedRig) -> support::Reply {
    add_and_wait(
        &m.api,
        &mut m.events,
        "adds",
        json!({"kind": "existing", "url": MOVED_REMOTE, "directory": "legacy"}),
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn managing_a_checkout_reads_its_remote_again_and_changes_nothing_in_it() {
    let mut m = managed_checkouts_rig().await;
    let clones = m.rig.guest.clone_commands().len();
    let commands = m.rig.guest.commands().len();
    move_the_legacy_remote(&m).await;
    let listings = m.rig.guest.listings.lock().unwrap().len();

    let reply = manage_legacy_at_its_new_remote(&mut m).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        m.rig
            .guest
            .listings
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .as_deref(),
        Some("legacy"),
        "its remote was read again for this call"
    );
    assert_eq!(m.rig.guest.listings.lock().unwrap().len(), listings + 1);
    let entries = reply.json()["repositories"].clone();
    assert_eq!(
        without_entries(&json!([entries[1]])),
        json!([{"url": MOVED_REMOTE, "directory": "legacy"}])
    );
    assert_eq!(
        m.rig.guest.clone_commands().len(),
        clones,
        "nothing was cloned"
    );
    assert_eq!(
        m.rig.guest.commands().len(),
        commands,
        "no command but the reads ran in the guest"
    );
    let git = m.api.get("/api/workspaces/adds/git").await.json();
    let rows: Vec<_> = git["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["repo"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        rows,
        ["api", "moved"],
        "the new row is the one read just now"
    );
    m.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn managing_a_checkout_whose_remote_was_swapped_is_refused_naming_both_and_adds_nothing() {
    let mut m = managed_checkouts_rig().await;
    move_the_legacy_remote(&m).await;

    // The user saw the old address: a swapped remote is refused, naming both, and adds nothing.
    let swapped = add_and_wait(
        &m.api,
        &mut m.events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/legacy.git", "directory": "legacy"}),
    )
    .await;
    assert_eq!(swapped.status, 409, "{}", swapped.body);
    assert!(
        swapped.body.contains("legacy.git") && swapped.body.contains(MOVED_REMOTE),
        "{}",
        swapped.body
    );
    let record = m.api.get("/api/workspaces/adds").await.json();
    assert_eq!(
        record["repositories"].as_array().unwrap().len(),
        1,
        "{record}"
    );
    m.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn managing_something_that_is_not_a_checkout_has_an_ssh_remote_or_is_listed_is_refused() {
    let mut m = managed_checkouts_rig().await;
    move_the_legacy_remote(&m).await;
    let reply = manage_legacy_at_its_new_remote(&mut m).await;
    assert_eq!(reply.status, 200, "{}", reply.body);

    // One that is not a checkout, one with an SSH remote and one already listed are refused.
    for dir in ["missing", "ssh", "legacy"] {
        let refused = add_and_wait(
            &m.api,
            &mut m.events,
            "adds",
            json!({"kind": "existing", "url": "https://example.org/acme/x.git", "directory": dir}),
        )
        .await;
        assert_eq!(refused.status, 409, "{dir}: {}", refused.body);
    }
    m.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_an_entry_keeps_the_checkout_its_table_row_and_its_identity() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    make_work_identity(&api).await;
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    let rows_before = api.get("/api/workspaces/adds/git").await.json();

    let api_entry = entry_of(&api, "adds", "api").await;
    let by_directory = api
        .delete("/api/workspaces/adds/repositories/api", None)
        .await;
    assert_eq!(by_directory.status, 404, "{}", by_directory.body);
    let reply = api
        .delete(
            &format!("/api/workspaces/adds/repositories/{api_entry}"),
            None,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["repositories"], json!([]));
    let list = checkouts_of(&api, "adds").await;
    let checkout = &list["checkouts"][0];
    assert_eq!(checkout["directory"], "api");
    assert_eq!(
        checkout["entry"],
        serde_json::Value::Null,
        "the checkout stays, off the list"
    );
    assert_eq!(
        api.get("/api/workspaces/adds/git").await.json(),
        rows_before
    );
    let gone = api
        .delete(
            &format!("/api/workspaces/adds/repositories/{api_entry}"),
            None,
        )
        .await;
    assert_eq!(gone.status, 404, "{}", gone.body);

    // The checkout can be managed again.
    let back = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/api.git", "directory": "api"}),
    )
    .await;
    assert_eq!(back.status, 200, "{}", back.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_is_cloning_cannot_be_stopped_or_asked_for_a_second_clone() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let hold = Mutex::new(hold);
    *rig.guest.at_clone.lock().unwrap() = Some(Box::new(move |_| {
        wait_for(&hold.lock().unwrap());
    }));

    let reply = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": WEB}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let stop = api.post("/api/workspaces/adds/stop", "").await;
    assert_eq!(stop.status, 409, "{}", stop.body);
    let clone_too = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": "https://example.org/acme/more"}).to_string(),
        )
        .await;
    assert_eq!(clone_too.status, 409, "{}", clone_too.body);
    assert!(
        clone_too.body.contains("busy (cloning)"),
        "{}",
        clone_too.body
    );
    rig.guest
        .put_checkout("legacy", "https://example.org/acme/legacy.git");
    let second = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/legacy.git", "directory": "legacy"}),
    )
    .await;
    assert_eq!(second.status, 409, "{}", second.body);
    let delete = api
        .delete(
            "/api/workspaces/adds",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(delete.status, 409, "{}", delete.body);

    release.send(()).unwrap();
    events.until(ended("adds"), Duration::from_secs(20)).await;
    assert_eq!(
        rig.guest.clone_commands().len(),
        2,
        "only the first add cloned"
    );
    let stop = api.post("/api/workspaces/adds/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended("adds"), Duration::from_secs(20)).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspace_lists_the_checkouts_it_had_and_refuses_an_add() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    // The listing after the start is made in the background: wait for it.
    eventually("the background listing", Duration::from_secs(10), || {
        !rig.guest.listings.lock().unwrap().is_empty()
    })
    .await;
    api.post("/api/workspaces/adds/stop", "").await;
    events.until(ended("adds"), Duration::from_secs(20)).await;
    let listings = rig.guest.listings.lock().unwrap().len();

    let list = checkouts_of(&api, "adds").await;
    assert_eq!(list["running"], false);
    assert!(
        list["problem"].as_str().unwrap().contains("not running"),
        "{list}"
    );
    assert_eq!(
        list["checkouts"][0]["directory"], "api",
        "the last listing is kept: {list}"
    );
    assert_eq!(
        rig.guest.listings.lock().unwrap().len(),
        listings,
        "no command ran"
    );

    let add = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "clone", "url": WEB}),
    )
    .await;
    assert_eq!(add.status, 409, "{}", add.body);
    // Taking an entry off the list works while stopped.
    let api_entry = entry_of(&api, "adds", "api").await;
    let removed = api
        .delete(
            &format!("/api/workspaces/adds/repositories/{api_entry}"),
            None,
        )
        .await;
    assert_eq!(removed.status, 200, "{}", removed.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_listing_says_why_a_remote_is_not_shown_and_a_checkout_it_cannot_read_cannot_be_managed()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    rig.guest
        .put_checkout("keyed", "https://me:secret@example.org/acme/keyed.git");
    rig.guest
        .put_checkout("plain", "http://example.org/acme/plain.git");
    rig.guest.put_checkout("bare", "");
    rig.guest
        .put_checkout("shell", "git@example.org:acme/shell.git");
    rig.guest
        .unreadable
        .lock()
        .unwrap()
        .push(("broken".to_owned(), "permission denied".to_owned()));
    // The host's clock counts milliseconds and a listing begun in the call's own millisecond counts
    // as fresh: the call has to start after the guest changed.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let list = checkouts_of(&api, "adds").await;
    let notes: Vec<_> = list["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["directory"].as_str().unwrap(), c["remote_note"].clone()))
        .collect();
    assert_eq!(
        notes,
        [
            ("api", json!(null)),
            ("bare", json!("no_remote")),
            ("broken", json!(null)),
            ("keyed", json!("credentials_removed")),
            ("plain", json!("not_https")),
            ("shell", json!("ssh")),
        ]
    );
    let keyed = &list["checkouts"][3];
    assert_eq!(keyed["remote_url"], "https://example.org/acme/keyed.git");
    assert!(!list.to_string().contains("secret"), "{list}");
    assert_eq!(list["checkouts"][2]["unreadable"], "permission denied");

    let refused = add_and_wait(
        &api,
        &mut events,
        "adds",
        json!({"kind": "existing", "url": "https://example.org/acme/broken.git", "directory": "broken"}),
    )
    .await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(
        refused.body.contains("permission denied"),
        "{}",
        refused.body
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_listing_the_guest_cannot_make_is_a_problem_in_the_answer_not_an_error() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    rig.guest.fail_listing.store(true, Ordering::SeqCst);
    // The host's clock counts milliseconds and a listing begun in the call's own millisecond counts
    // as fresh: the call has to start after the guest changed.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let list = checkouts_of(&api, "adds").await;
    assert_eq!(list["running"], true);
    assert!(
        list["problem"].as_str().unwrap().contains("cannot enter"),
        "{list}"
    );
    assert_eq!(list["checked_at"], json!(null));
    assert_eq!(list["checkouts"], json!([]));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn calls_that_wait_for_a_listing_share_the_one_that_starts_after_they_asked() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api0 = api(&host);
    let mut events = api0.events().await;
    create(&api0, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let before = rig.guest.listings.lock().unwrap().len();
    let (release, hold) = std::sync::mpsc::channel::<()>();
    *rig.guest.listing_hold.lock().unwrap() = Some(hold);

    let ask = |host: &Host<FakeRuntime>| {
        let api = api(host);
        tokio::spawn(async move { checkouts_of(&api, "adds").await })
    };
    let first = ask(&host);
    eventually("the listing is held", Duration::from_secs(10), || {
        rig.guest.listing_waits.load(Ordering::SeqCst)
    })
    .await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let (second, third) = (ask(&host), ask(&host));
    tokio::time::sleep(Duration::from_millis(200)).await;
    release.send(()).unwrap();
    let (first, second, third) = (
        first.await.unwrap(),
        second.await.unwrap(),
        third.await.unwrap(),
    );
    assert_eq!(
        rig.guest.listings.lock().unwrap().len(),
        before + 2,
        "the held listing, and one for the two that waited behind it"
    );
    assert_eq!(second, third);
    assert!(second["checked_at"].as_u64() >= first["checked_at"].as_u64());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_list_that_cannot_be_saved_changes_nothing_and_says_so() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    let api_entry = entry_of(&api, "adds", "api").await;
    make_unwritable(&list_path(&rig));

    let reply = api
        .delete(
            &format!("/api/workspaces/adds/repositories/{api_entry}"),
            None,
        )
        .await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("nothing changed"), "{}", reply.body);
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 1, "{ws}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_switch_that_cannot_be_saved_or_comes_at_shutdown_changes_nothing() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "who").await;
    let body = json!({"session_user": "root"}).to_string();
    let switch = || api.put("/api/workspaces/who/session-user", &body);
    make_unwritable(&list_path(&rig));
    let reply = switch().await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("was not changed"), "{}", reply.body);
    let ws = api.get("/api/workspaces/who").await.json();
    assert_eq!(ws["session_user"], "agent", "{ws}");
    host.workspaces().close();
    let reply = switch().await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("shutting down"), "{}", reply.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_is_added_once_shutdown_has_begun() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    host.workspaces().close();
    let reply = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": WEB}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("shutting down"), "{}", reply.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_cannot_be_removed_while_its_workspace_is_being_created() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let hold = Mutex::new(hold);
    *rig.guest.at_clone.lock().unwrap() = Some(Box::new(move |_| {
        wait_for(&hold.lock().unwrap());
    }));
    let reply = api.post("/api/workspaces", &new_workspace("late")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let late_entry = entry_of(&api, "late", "api").await;
    let refused = api
        .delete(
            &format!("/api/workspaces/late/repositories/{late_entry}"),
            None,
        )
        .await;
    assert_eq!(refused.status, 409, "{}", refused.body);
    assert!(refused.body.contains("creating"), "{}", refused.body);
    release.send(()).unwrap();
    events.until(ended("late"), Duration::from_secs(20)).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clone_the_runtime_cannot_run_fails_the_operation_and_keeps_the_entry() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "adds").await;
    after_boot_listing(&rig).await;
    // The add looks for the folder first; the next command is the clone.
    rig.runtime.inject(
        Op::Exec,
        Fault::once(disk_error("exec")).only_for("adds").after(1),
    );
    let reply = api
        .post(
            "/api/workspaces/adds/repositories",
            &json!({"kind": "clone", "url": WEB}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("adds"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("no space left"),
        "{end}"
    );
    let ws = api.get("/api/workspaces/adds").await.json();
    assert_eq!(ws["repositories"].as_array().unwrap().len(), 2, "{ws}");
    assert_eq!(ws["busy"], json!(null));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_with_no_repositories_is_made_without_a_clone_or_a_sync() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .post("/api/workspaces", &json!({"name": "bare"}).to_string())
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("bare"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        events.steps("bare"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "syncing",
            "done"
        ]
    );
    assert_eq!(rig.guest.clone_commands(), Vec::<String>::new());
    assert_eq!(rig.guest.syncs(), 0);
    let ws = api.get("/api/workspaces/bare").await.json();
    assert_eq!(
        (&ws["status"], &ws["repositories"]),
        (&json!("running"), &json!([]))
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_stops_answering_between_clones_fails_the_create_and_leaves_nothing() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let body = |name: &str| {
        json!({"name": name, "repositories": [
            {"url": REPO}, {"url": "https://example.org/acme/web"}
        ]})
        .to_string()
    };
    // A create that works shows how many commands a create runs before its second clone.
    api.post("/api/workspaces", &body("probe")).await;
    events.until(ended("probe"), Duration::from_secs(20)).await;
    let before_second_clone = rig
        .runtime
        .calls()
        .iter()
        .filter(|c| c.op == Op::Exec && c.target.as_deref() == Some("probe"))
        .take_while(|c| {
            c.detail.as_deref()
                != Some("git clone -- https://example.org/acme/web /workspaces/probe/web")
        })
        .count();
    rig.runtime.inject(
        Op::Exec,
        Fault::once(disk_error("exec"))
            .only_for("gone")
            .after(u32::try_from(before_second_clone).unwrap()),
    );
    let clones_before = rig.guest.clone_commands().len();
    let reply = api.post("/api/workspaces", &body("gone")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("gone"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"].as_str().unwrap().contains("no space left"),
        "{end}"
    );
    assert_eq!(
        rig.guest.clone_commands()[clones_before..],
        [format!("clone -- {REPO} /workspaces/gone/api")],
        "the second clone never reached the guest"
    );
    assert_eq!(api.get("/api/workspaces/gone").await.status, 404);
    let volumes: Vec<String> = rig
        .runtime
        .list_volumes()
        .await
        .unwrap()
        .into_iter()
        .map(|v| v.name)
        .collect();
    assert_eq!(volumes, ["ws-probe"], "the failed create's volume is gone");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_on_a_version_1_list_shows_one_repository_in_its_checkout_and_a_rename_saves_version_3()
 {
    let rig = Rig::new();
    let data = rig.dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let path = data.join("workspaces.json");
    let v1 = json!({"version": 1, "workspaces": [{
        "id": "acme", "name": "acme", "repo_url": "https://example.org/acme/shop.git",
        "image": FakeRuntime::DEBIAN, "memory_mib": 1024, "created_at": 1, "disk_size_mib": 1024,
        "first_connect_notice_due": false
    }]})
    .to_string();
    std::fs::write(&path, &v1).unwrap();
    let host = rig.start().await;
    let api = api(&host);
    let ws = api.get("/api/workspaces/acme").await.json();
    assert_eq!(
        without_entries(&ws["repositories"]),
        json!([{"url": "https://example.org/acme/shop.git", "directory": "shop"}])
    );
    assert_eq!(ws["start_folder"], "shop");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        v1,
        "a start never writes the list"
    );
    let renamed = api
        .patch("/api/workspaces/acme", &json!({"name": "Shop"}).to_string())
        .await;
    assert_eq!(renamed.status, 200, "{}", renamed.body);
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["version"], 4);
    assert_eq!(
        without_entries(&saved["workspaces"][0]["repositories"]),
        json!([{"url": "https://example.org/acme/shop.git", "directory": "shop"}])
    );
    assert_eq!(
        saved["workspaces"][0]["repositories"][0]["entry"], ws["repositories"][0]["entry"],
        "the id given on read is the one saved"
    );
    assert_eq!(saved["workspaces"][0]["start_folder"], "shop");
    assert_eq!(saved["workspaces"][0]["name"], "Shop");
    assert!(
        saved["workspaces"][0].get("display_name").is_none(),
        "{saved}"
    );
    assert!(saved["workspaces"][0].get("repo_url").is_none(), "{saved}");
    host.shutdown().await;
}
