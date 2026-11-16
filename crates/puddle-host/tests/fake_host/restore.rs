// SPDX-License-Identifier: AGPL-3.0-or-later
//! Restoring a workspace from a disk the list does not name.

use super::*;

async fn add_unclaimed_volume(rig: &Rig, id: &str) {
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new(id).unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
}

async fn orphan_ids(api: &Api) -> Vec<String> {
    let reply = api.get("/api/orphan-volumes").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply.json()["volumes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_owned())
        .collect()
}

fn orphan_problem(problems: &[(String, Option<String>, String, String)]) -> Option<&str> {
    problems
        .iter()
        .find(|p| p.0 == "orphan-volumes")
        .map(|p| p.3.as_str())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disk_the_list_does_not_name_is_noticed_and_its_notice_never_says_it_holds_data() {
    let rig = Rig::new();
    add_unclaimed_volume(&rig, "lost").await;
    add_unclaimed_volume(&rig, "other").await;
    let host = rig.start().await;
    let api = api(&host);

    assert_eq!(orphan_ids(&api).await, ["lost", "other"]);
    let problems = problems_of(&api).await;
    let detail = orphan_problem(&problems).expect("a notice");
    assert!(detail.contains("ws-lost, ws-other"), "{detail}");
    assert!(detail.contains("cannot tell from outside"), "{detail}");
    assert!(!detail.contains("holds data"), "{detail}");
    // Noticed, never removed.
    assert_eq!(volume_names(&rig).await, ["ws-lost", "ws-other"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_makes_a_working_list_entry_with_the_disk_untouched_and_ends_its_notice() {
    let rig = Rig::new();
    add_unclaimed_volume(&rig, "lost").await;
    add_unclaimed_volume(&rig, "other").await;
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    let host = rig.start().await;
    let first = api(&host);
    let before = rig.guest.volume.lock().unwrap().clone();

    let restored = first.post("/api/orphan-volumes/lost/restore", "{}").await;
    assert_eq!(restored.status, 201, "{}", restored.body);
    let made = restored.json();
    // The id is the disk's; the name is the folder of its first checkout.
    assert_eq!(made["id"], "lost");
    assert_eq!(made["name"], "api");
    assert_eq!(made["status"], "stopped");
    assert_eq!(
        made["repositories"][0]["url"],
        "https://example.org/acme/api.git"
    );
    assert_eq!(made["repositories"][0]["directory"], "api");

    // Nothing was written to the disk or the volume, and nothing was cloned.
    assert_eq!(*rig.guest.volume.lock().unwrap(), before);
    assert!(rig.guest.clone_commands().is_empty());
    assert_eq!(volume_names(&rig).await, ["ws-lost", "ws-other"]);
    // The maintenance sandbox that read it is gone again.
    assert!(rig.runtime.list().await.unwrap().is_empty());

    // The disk left the notice; the other one stays in it.
    assert_eq!(orphan_ids(&first).await, ["other"]);
    let problems = problems_of(&first).await;
    let detail = orphan_problem(&problems).expect("a notice for the other disk");
    assert!(
        detail.contains("ws-other") && !detail.contains("ws-lost"),
        "{detail}"
    );

    // The entry is a workspace like any other: it starts, and it is in the saved list.
    let mut events = first.events().await;
    first.post("/api/workspaces/lost/start", "").await;
    events.until(ended("lost"), Duration::from_secs(20)).await;
    assert_eq!(
        status_of(&first.get("/api/workspaces/lost").await),
        "running"
    );
    first.post("/api/workspaces/lost/stop", "").await;
    events.until(ended("lost"), Duration::from_secs(20)).await;
    host.shutdown().await;
    drop(host);
    let again = rig.start().await;
    let api2 = api(&again);
    let listed = api2.get("/api/workspaces").await.json();
    assert_eq!(listed["workspaces"][0]["name"], "api", "{listed}");
    assert_eq!(orphan_ids(&api2).await, ["other"]);
    // A second disk whose first checkout has the same folder: the name gets a number.
    let other = api2.post("/api/orphan-volumes/other/restore", "{}").await;
    assert_eq!(other.status, 201, "{}", other.body);
    assert_eq!(other.json()["name"], "api (2)", "{}", other.body);
    again.shutdown().await;
}

/// A restore boots the same maintenance image a delete check does, so it waits for that image's
/// download like the check: nothing boots before the pull of the image has been asked for.
#[tokio::test(flavor = "multi_thread")]
async fn a_restore_asks_for_the_maintenance_image_before_it_boots_it() {
    const CHECK_IMAGE: &str = puddle_workspace::WorkspaceConfig::DEFAULT_MAINTENANCE_IMAGE;
    let rig = Rig::new();
    add_unclaimed_volume(&rig, "lost").await;
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    let host = rig.start().await;
    let api = api(&host);
    let restored = api.post("/api/orphan-volumes/lost/restore", "{}").await;
    assert_eq!(restored.status, 201, "{}", restored.body);
    let calls = rig.runtime.calls();
    let pulled = calls
        .iter()
        .position(|c| c.op == Op::PullImage && c.target.as_deref() == Some(CHECK_IMAGE));
    let booted = calls.iter().position(|c| {
        c.op == Op::Create && c.target.as_deref().is_some_and(|t| t.starts_with("m--"))
    });
    assert!(
        pulled.is_some() && booted.is_some() && pulled < booted,
        "pull at {pulled:?}, maintenance boot at {booted:?}: {calls:?}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_whose_list_cannot_be_saved_leaves_no_entry_and_the_disk_unclaimed() {
    let rig = Rig::new();
    add_unclaimed_volume(&rig, "lost").await;
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    let host = rig.start().await;
    let api = api(&host);
    make_unwritable(&list_path(&rig));

    let reply = api.post("/api/orphan-volumes/lost/restore", "{}").await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("was not restored"), "{}", reply.body);
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(orphan_ids(&api).await, ["lost"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_asks_for_an_address_when_the_disk_has_none_and_the_last_one_ends_the_notice() {
    let rig = Rig::new();
    add_unclaimed_volume(&rig, "bare").await;
    let host = rig.start().await;
    let api = api(&host);

    let asked = api.post("/api/orphan-volumes/bare/restore", "{}").await;
    assert_eq!(asked.status, 422, "{}", asked.body);
    assert!(asked.body.contains("enter the address"), "{}", asked.body);
    // The refusal made no entry and left the disk where it was.
    assert_eq!(orphan_ids(&api).await, ["bare"]);
    assert!(
        api.get("/api/workspaces").await.json()["workspaces"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let given = api
        .post(
            "/api/orphan-volumes/bare/restore",
            r#"{"repository": "https://example.org/acme/web"}"#,
        )
        .await;
    assert_eq!(given.status, 201, "{}", given.body);
    assert_eq!(given.json()["repositories"][0]["directory"], "web");
    assert!(orphan_ids(&api).await.is_empty());
    assert!(orphan_problem(&problems_of(&api).await).is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restore_never_overwrites_a_list_entry_or_takes_a_disk_a_sandbox_holds() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api1 = api(&host);
    let mut events = api1.events().await;
    create(&api1, &mut events, "acme").await;
    let before = api1.get("/api/workspaces/acme").await.json();

    // A listed workspace's disk is not unclaimed, and asking to restore it changes nothing.
    assert!(orphan_ids(&api1).await.is_empty());
    let clash = api1
        .post(
            "/api/orphan-volumes/acme/restore",
            r#"{"repository": "https://example.org/other/repo"}"#,
        )
        .await;
    assert_eq!(clash.status, 409, "{}", clash.body);
    assert!(
        clash.body.contains("nothing was replaced"),
        "{}",
        clash.body
    );
    assert_eq!(api1.get("/api/workspaces/acme").await.json(), before);

    // Two restores of one disk at once: one wins, the other is refused, one entry results.
    add_unclaimed_volume(&rig, "twice").await;
    let body = r#"{"repository": "https://example.org/acme/twice"}"#;
    let (a, b) = tokio::join!(
        api1.post("/api/orphan-volumes/twice/restore", body),
        api1.post("/api/orphan-volumes/twice/restore", body)
    );
    let mut statuses = [a.status, b.status];
    statuses.sort_unstable();
    assert_eq!(statuses, [201, 409], "{} / {}", a.body, b.body);
    let all = api1.get("/api/workspaces").await.json();
    let names: Vec<_> = all["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["name"].as_str().unwrap().to_owned())
        .collect();
    // The disk's first checkout names the entry (this fake guest lists one set for every disk).
    assert_eq!(names, ["acme", "api"]);

    // No such disk.
    let none = api1.post("/api/orphan-volumes/nothing/restore", "{}").await;
    assert_eq!(none.status, 404, "{}", none.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_disk_that_cannot_be_read_makes_no_entry() {
    let rig = Rig::new();
    add_unclaimed_volume(&rig, "stuck").await;
    rig.guest.fail_listing.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);

    let reply = api
        .post(
            "/api/orphan-volumes/stuck/restore",
            r#"{"repository": "https://example.org/acme/stuck"}"#,
        )
        .await;
    assert_eq!(reply.status, 503, "{}", reply.body);
    assert!(reply.body.contains("ws-stuck"), "{}", reply.body);
    assert_eq!(orphan_ids(&api).await, ["stuck"]);
    assert_eq!(volume_names(&rig).await, ["ws-stuck"]);
    assert!(rig.runtime.list().await.unwrap().is_empty());
    host.shutdown().await;
}
