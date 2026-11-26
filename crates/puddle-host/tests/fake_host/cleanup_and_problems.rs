// SPDX-License-Identifier: AGPL-3.0-or-later
//! Cleanups that fail, trims at stop and quit, and the reconcile list.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_sync_that_cannot_be_cleaned_up_names_what_is_left_in_the_failure() {
    let rig = Rig::named_ids();
    rig.guest.fail_sync.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime
        .inject(Op::RemoveVolume, Fault::always(disk_error("remove volume")));
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("sync: I/O error"), "the cause: {detail}");
    assert!(detail.contains("could not clean up"), "{detail}");
    assert!(
        detail.contains("the new volume ws-"),
        "what is left: {detail}"
    );
    assert!(detail.contains("no space left"), "why: {detail}");
    assert!(detail.contains("next start"), "the way out: {detail}");
    host.shutdown().await;
}

/// What the real registry client reports when a TLS-intercepting company proxy re-signs the
/// registry's certificate and Puddle does not trust the proxy's root (the lab's own run:
/// `image_pull_intercepting_proxy.rs`, whose test pins that this is the text a real pull makes).
const INTERCEPTED_PULL: &str = "registry error: error sending request for url (https://registry.corp.test/v2/): client error (Connect): invalid peer certificate: UnknownIssuer";

#[tokio::test(flavor = "multi_thread")]
async fn a_pull_that_fails_on_a_company_certificate_ends_the_create_with_that_text_for_the_user() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime.inject(
        Op::PullImage,
        Fault::always(puddle_compute::ComputeError::ImagePull {
            image: "registry.corp.test/acme/base:1".into(),
            reason: INTERCEPTED_PULL.into(),
        }),
    );
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(
        detail.contains("could not be pulled") && detail.contains("certificate"),
        "the user reads the certificate, not a bare network error: {detail}"
    );
    assert!(detail.contains("UnknownIssuer"), "{detail}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_boot_that_cannot_be_cleaned_up_names_what_is_left_in_the_failure() {
    let rig = Rig::named_ids();
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    rig.runtime
        .inject(Op::RemoveVolume, Fault::always(disk_error("remove volume")));
    api.post("/api/workspaces", &new_workspace("acme")).await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(detail.contains("boot hook"), "the cause: {detail}");
    assert!(detail.contains("could not clean up"), "{detail}");
    assert!(detail.contains("no space left"), "why: {detail}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_git_locks_that_could_not_be_cleared_are_a_problem_until_a_boot_clears_them() {
    let rig = Rig::named_ids();
    *rig.guest.locks_answer.lock().unwrap() = Some((3, "E\t.\tcannot enter /workspace\n"));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let problems = problems_of(&api).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    let (key, workspace, title, detail) = &problems[0];
    assert_eq!(key, "git-locks:acme");
    assert_eq!(workspace.as_deref(), Some("acme"));
    assert!(title.contains("stale git lock"), "{title}");
    assert!(detail.contains("cannot enter"), "the reason: {detail}");
    assert!(detail.contains("deleted by hand"), "the way out: {detail}");

    *rig.guest.locks_answer.lock().unwrap() = None;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(problems_of(&api).await, []);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trim_that_failed_at_stop_and_a_stop_that_had_to_be_forced_are_problems_the_next_run_ends()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    rig.guest.fail_trim.store(true, Ordering::SeqCst);
    rig.runtime
        .force_next_stop(&SandboxName::new("acme").unwrap());
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let problems = problems_of(&api).await;
    let keys: Vec<_> = problems.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(keys, ["forced-stop:acme", "trim:acme"], "{problems:?}");
    assert!(problems[0].3.contains("not written"), "{problems:?}");
    assert!(
        problems[1].3.contains("not supported"),
        "the reason: {problems:?}"
    );
    assert!(
        problems[1].3.contains("Reclaim space"),
        "the way out: {problems:?}"
    );

    // Starting it again ends the forced stop; a stop whose trim works ends the other.
    rig.guest.fail_trim.store(false, Ordering::SeqCst);
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let keys: Vec<_> = problems_of(&api).await.into_iter().map(|p| p.0).collect();
    assert_eq!(keys, ["trim:acme"]);
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(problems_of(&api).await, []);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trim_that_failed_when_puddle_quit_is_a_problem_at_the_next_start_until_a_trim_works() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    rig.guest.fail_trim.store(true, Ordering::SeqCst);
    host.shutdown().await;
    drop(host);
    drop(events);
    drop(api);

    let host = rig.start().await;
    let api = self::api(&host);
    let problems = problems_of(&api).await;
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].0, "trim:acme");
    assert_eq!(problems[0].1.as_deref(), Some("acme"));
    assert!(problems[0].2.contains("when Puddle quit"), "{problems:?}");
    assert!(problems[0].3.contains("not supported"), "{problems:?}");
    assert!(problems[0].3.contains("Reclaim space"), "{problems:?}");

    // Quitting again with the workspace stopped trims nothing; the note stays until a trim works.
    host.shutdown().await;
    drop(host);
    let host = rig.start().await;
    let api = self::api(&host);
    let keys: Vec<_> = problems_of(&api).await.into_iter().map(|p| p.0).collect();
    assert_eq!(keys, ["trim:acme"]);

    // A start and a stop whose trim works end it, and the next start has nothing to say.
    rig.guest.fail_trim.store(false, Ordering::SeqCst);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(problems_of(&api).await, []);
    host.shutdown().await;
    drop(host);
    let host = rig.start().await;
    assert_eq!(problems_of(&self::api(&host)).await, []);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quit_whose_trim_works_leaves_no_note_and_a_foreign_note_file_is_ignored() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    host.shutdown().await;
    drop(host);
    let note = rig.config().paths.quit_trims();
    assert!(!note.exists(), "a clean quit writes no note");

    std::fs::write(&note, r#"{"version":9,"untrimmed":{"acme":"x"}}"#).unwrap();
    let host = rig.start().await;
    assert_eq!(problems_of(&self::api(&host)).await, []);
    host.shutdown().await;
    assert!(!note.exists(), "the quit rewrote the file from what stood");
}

#[tokio::test(flavor = "multi_thread")]
async fn what_reconcile_kept_removed_or_could_not_do_at_start_is_listed_with_the_way_out() {
    let rig = Rig::new();
    rig.runtime
        .create_volume(puddle_compute::VolumeSpec {
            name: WorkspaceId::new("lost").unwrap().volume_name(),
            size: puddle_compute::DiskSize::mib(1024),
        })
        .await
        .unwrap();
    for ghost in ["ghost", "stuck"] {
        rig.runtime
            .create(puddle_compute::SandboxSpec::new(
                SandboxName::new(ghost).unwrap(),
                ImageRef::new(FakeRuntime::DEBIAN).unwrap(),
            ))
            .await
            .unwrap();
    }
    // "stuck" cannot be removed: the record it has is locked.
    rig.runtime.inject(
        Op::Remove,
        Fault::always(puddle_compute::ComputeError::Runtime {
            op: "remove",
            message: "the record is locked".into(),
        })
        .only_for("stuck"),
    );

    let host = rig.start().await;
    let problems = problems_of(&api(&host)).await;
    let keys: Vec<_> = problems.iter().map(|p| p.0.as_str()).collect();
    assert_eq!(
        keys,
        [
            "orphan-volumes",
            "reconcile-failures",
            "reconcile-removed-sandboxes"
        ],
        "{problems:?}"
    );
    assert!(problems[1].3.contains("stuck"), "{problems:?}");
    assert!(problems[1].3.contains("record is locked"), "{problems:?}");
    assert!(problems[2].3.contains("ghost"), "{problems:?}");
    assert!(problems[2].3.contains("untouched"), "{problems:?}");
    assert!(problems[0].3.contains("ws-lost"), "{problems:?}");
    assert!(problems[0].3.contains("changed nothing"), "{problems:?}");
    host.shutdown().await;
}
