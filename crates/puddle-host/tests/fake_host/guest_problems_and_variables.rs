// SPDX-License-Identifier: AGPL-3.0-or-later
//! Problems the guest raises and variables set around a boot.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn an_entrypoint_that_failed_at_once_is_a_problem_until_a_start_where_it_did_not() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: entrypoint exited with status 127: dockerd: not found\n".to_owned();
    create(&api, &mut events, "acme").await;
    let problem = host
        .problems()
        .list()
        .into_iter()
        .find(|p| p.key == "entrypoint:acme")
        .unwrap_or_else(|| panic!("no problem: {:?}", host.problems().list()));
    assert_eq!(problem.title, "The image's entrypoint in acme stopped");
    assert!(
        problem.detail.contains("status 127: dockerd: not found")
            && problem.detail.contains("/run/puddle/entrypoint.log"),
        "{}",
        problem.detail
    );
    assert_eq!(
        problem.workspace.as_ref().map(WorkspaceId::as_str),
        Some("acme")
    );

    // The next start, where it did not fail, takes the problem away.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems()
            .list()
            .iter()
            .all(|p| p.key != "entrypoint:acme"),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

/// The id a real host draws is eight random characters, and the user never sees it: a problem's
/// title and detail name the workspace as the user wrote it.
#[tokio::test(flavor = "multi_thread")]
async fn problem_text_names_a_workspace_by_its_display_name_never_by_its_random_id() {
    let rig = Rig::new();
    *rig.ids.lock().unwrap() = Some(Arc::new(puddle_api::RandomIds));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: entrypoint exited with status 127: dockerd: not found\n\
         puddle-boot: session user cannot become root: no sudo\n"
            .to_owned();
    let reply = api
        .post("/api/workspaces", &new_workspace("Acme shop"))
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let id = reply.json()["id"].as_str().unwrap().to_owned();
    assert_ne!(id, "acme-shop", "{id}");
    events.until(ended(&id), Duration::from_secs(20)).await;
    let problems = host.problems().list();
    let titles: Vec<_> = problems.iter().map(|p| p.title.as_str()).collect();
    assert!(
        titles.contains(&"Acme shop has no sudo, so its user cannot become root"),
        "{titles:?}"
    );
    assert!(
        titles.contains(&"The image's entrypoint in Acme shop stopped"),
        "{titles:?}"
    );
    for problem in &problems {
        assert!(
            !problem.title.contains(&id) && !problem.detail.contains(&id),
            "the random id {id} is in a problem the user reads: {problem:?}"
        );
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_with_no_sudo_is_a_problem_until_a_start_that_finds_sudo() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: session user agent (uid 1000), no sudo in this image\n\
         puddle-boot: session user cannot become root: no sudo\n"
            .to_owned();
    create(&api, &mut events, "acme").await;
    let problem = host
        .problems()
        .list()
        .into_iter()
        .find(|p| p.key == "no-sudo:acme")
        .unwrap_or_else(|| panic!("no problem: {:?}", host.problems().list()));
    assert_eq!(
        problem.title,
        "acme has no sudo, so its user cannot become root"
    );
    assert!(
        problem.detail.contains("use an image that has `sudo`")
            && problem.detail.contains("session user to root"),
        "{}",
        problem.detail
    );
    assert_eq!(
        problem.workspace.as_ref().map(WorkspaceId::as_str),
        Some("acme")
    );

    // It stays through a stop, and a start that says nothing about sudo (sudo found, or a root
    // session user) takes it away.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems()
            .list()
            .iter()
            .any(|p| p.key == "no-sudo:acme"),
        "{:?}",
        host.problems().list()
    );
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems()
            .list()
            .iter()
            .all(|p| p.key != "no-sudo:acme"),
        "{:?}",
        host.problems().list()
    );

    // A delete takes a problem that is still up away with the workspace.
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: session user cannot become root: no sudo\n".to_owned();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems()
            .list()
            .iter()
            .any(|p| p.key == "no-sudo:acme")
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let bare = json!({"confirm": true}).to_string();
    let accepted = api.delete("/api/workspaces/acme", Some(&bare)).await;
    assert_eq!(accepted.status, 202, "{}", accepted.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        host.problems()
            .list()
            .iter()
            .all(|p| p.key != "no-sudo:acme"),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_removes_its_shell_init_and_the_folder() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    put_shell_init(&api, "acme", "export A=1\n").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(shell_init_folder(&rig, "acme").exists());
    let reply = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!shell_init_folder(&rig, "acme").exists());
    assert_eq!(host.store().shell_init(&name("acme")).unwrap(), None);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_variable_reaches_the_login_shell_file_of_a_running_workspace_without_a_restart()
{
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let boots = rig.guest.boots();
    let (creates, starts, stops) = (
        calls_of(&rig, "Create"),
        calls_of(&rig, "Start"),
        calls_of(&rig, "Stop"),
    );

    put_env(
        &api,
        "/api/workspaces/acme/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    rig.guest.boots_reach(boots + 1).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");

    // A global variable reaches it too, a secret as its stand-in and never its value, and a
    // removed one leaves the file.
    put_env(
        &api,
        "/api/env/NPM_TOKEN",
        secret_body(Some(REAL), &["registry.example.org"]),
    )
    .await;
    rig.guest.boots_reach(boots + 2).await;
    let stand_in = stand_in_of(&host, "acme", "NPM_TOKEN");
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(
        file.contains(&format!("export NPM_TOKEN='{stand_in}'")),
        "{file}"
    );
    assert!(!guest_has(&rig, REAL));
    assert_eq!(
        api.delete("/api/workspaces/acme/env/EDITOR", None)
            .await
            .status,
        204
    );
    rig.guest.boots_reach(boots + 3).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(!file.contains("EDITOR"), "{file}");

    // Nothing was created, started or stopped for any of it.
    assert_eq!(
        (
            calls_of(&rig, "Create"),
            calls_of(&rig, "Start"),
            calls_of(&rig, "Stop")
        ),
        (creates, starts, stops)
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_check_that_cannot_run_is_not_a_verdict_on_the_script() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // Saving: the guest could not set the check up (exit 70). The script is not blamed: the save
    // is answered as unavailable and nothing is saved.
    *rig.guest.check_answer.lock().unwrap() = Some((70, ""));
    let saved = put_shell_init(&api, "acme", "export A=1\n").await;
    assert_eq!(saved.status, 503, "{}", saved.body);
    assert!(saved.body.contains("could not run"), "{}", saved.body);
    assert_eq!(
        api.get("/api/workspaces/acme/shell-init").await.json()["saved"],
        false
    );

    // A runtime that fails the check's exec is no verdict either, and says why.
    *rig.guest.check_answer.lock().unwrap() = None;
    rig.runtime
        .inject(Op::Exec, Fault::once(disk_error("exec")).only_for("acme"));
    let saved = put_shell_init(&api, "acme", "export A=1\n").await;
    assert_eq!(saved.status, 503, "{}", saved.body);
    assert!(saved.body.contains("no space left"), "{}", saved.body);

    // Starting: the same failure is a problem on the workspace and the start goes on.
    *rig.guest.check_answer.lock().unwrap() = None;
    put_shell_init(&api, "acme", "export A=1\n").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    *rig.guest.check_answer.lock().unwrap() = Some((70, ""));
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    let problems = problems_of(&api).await;
    let found = problems.iter().find(|p| p.0 == "shell-init:acme");
    assert!(found.is_some(), "{problems:?}");
    assert!(found.unwrap().2.contains("could not check"), "{found:?}");
    host.shutdown().await;
}

/// Puts a plain file where the workspace's shell init folder is (or was), so nothing can be
/// published there.
fn block_shell_init_folder(rig: &Rig, workspace: &str) -> PathBuf {
    let folder = shell_init_folder(rig, workspace);
    std::fs::remove_dir_all(&folder).unwrap();
    std::fs::write(&folder, "in the way").unwrap();
    folder
}

#[tokio::test(flavor = "multi_thread")]
async fn a_shell_init_that_cannot_reach_the_running_guest_is_a_problem_until_a_save_gets_through() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let folder = block_shell_init_folder(&rig, "acme");

    let saved = put_shell_init(&api, "acme", "export A=1\n").await;
    assert_eq!(saved.status, 200, "{}", saved.body);
    let (title, detail) = problem_is(&api, "shell-init-copy:acme", true)
        .await
        .unwrap();
    assert!(title.contains("acme's shell init"), "{title}");
    assert!(detail.contains("keeps the shell init it had"), "{detail}");

    std::fs::remove_file(&folder).unwrap();
    put_shell_init(&api, "acme", "export A=2\n").await;
    problem_is(&api, "shell-init-copy:acme", false).await;
    assert_eq!(
        std::fs::read_to_string(folder.join("init.sh")).unwrap(),
        "export A=2\n"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_cannot_write_the_shell_init_folder_fails_and_leaves_the_workspace_stopped() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let folder = block_shell_init_folder(&rig, "acme");

    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"]
            .as_str()
            .unwrap()
            .contains("cannot write the shell init"),
        "{end}"
    );

    std::fs::remove_file(&folder).unwrap();
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_to_the_variables_that_the_guest_cannot_take_says_so_and_keeps_the_change() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    put_env(
        &api,
        "/api/workspaces/acme/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    let failed = events
        .until(
            is_event("workspace_env_failed", "acme"),
            Duration::from_secs(20),
        )
        .await;
    let reason = failed["reason"].as_str().unwrap();
    assert!(
        reason.contains("the change is saved") && reason.contains("keep the old variables"),
        "{failed}"
    );
    let saved = api.get("/api/workspaces/acme/env").await;
    assert!(saved.body.contains("EDITOR"), "{}", saved.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_cannot_write_the_shell_init_folder_fails_and_leaves_nothing_behind() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
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
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());

    std::fs::remove_file(&parent).unwrap();
    create_as(&api, &mut events, "acme", "acme-2").await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_variable_set_while_the_guest_boots_reaches_its_login_shells_once_it_is_up() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let store = host.store().clone();
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        store
            .set_env(
                puddle_store::UserId::OWNER,
                &puddle_store::EnvScope::Workspace(name("acme")),
                &puddle_store::EnvName::new("EDITOR").unwrap(),
                puddle_store::EnvDraft::plain("vim").unwrap(),
            )
            .unwrap();
    }));
    create(&api, &mut events, "acme").await;

    rig.guest.boots_reach(2).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");
    host.shutdown().await;
}

/// A change made from another thread while the boot hook is still busy reaches the host's change
/// handler before the hook returns: at that moment the host has no entry for the sandbox yet.
fn during_the_next_boot_from_another_thread(rig: &Rig, change: impl FnOnce() + Send + 'static) {
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        std::thread::spawn(change).join().unwrap();
        // Simulated busy boot hook: the change has landed before the hook returns.
        std::thread::sleep(Duration::from_millis(300));
    }));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_variable_set_before_the_first_boot_returns_reaches_the_guest_once_it_is_up() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let store = host.store().clone();
    during_the_next_boot_from_another_thread(&rig, move || {
        store
            .set_env(
                puddle_store::UserId::OWNER,
                &puddle_store::EnvScope::Workspace(name("acme")),
                &puddle_store::EnvName::new("EDITOR").unwrap(),
                puddle_store::EnvDraft::plain("vim").unwrap(),
            )
            .unwrap();
    });
    create(&api, &mut events, "acme").await;

    rig.guest.boots_reach(2).await;
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_variable_set_while_a_stopped_workspace_boots_again_reaches_the_guest_once_it_is_up() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let booted = rig.guest.boots();
    let store = host.store().clone();
    during_the_next_boot_from_another_thread(&rig, move || {
        store
            .set_env(
                puddle_store::UserId::OWNER,
                &puddle_store::EnvScope::Workspace(name("acme")),
                &puddle_store::EnvName::new("EDITOR").unwrap(),
                puddle_store::EnvDraft::plain("vim").unwrap(),
            )
            .unwrap();
    });

    let start = api.post("/api/workspaces/acme/start", "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;

    rig.guest.boots_reach(booted + 2).await;
    let restarted = pem_of(
        &rig.guest.plan_files(booted),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(!restarted.contains("EDITOR"), "{restarted}");
    let rerun = pem_of(
        &rig.guest.plan_files(booted + 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(rerun.contains("export EDITOR='vim'"), "{rerun}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_identity_attached_before_the_first_boot_returns_reaches_the_guest_once_it_is_up() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let store = host.store().clone();
    during_the_next_boot_from_another_thread(&rig, move || {
        attach_a_new_identity(&store, &name("acme"));
    });
    create(&api, &mut events, "acme").await;

    rig.guest.boots_reach(2).await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_run_in_a_live_guest_tells_the_user_when_it_put_the_loader_back() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();

    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: replaced /etc/profile.d/02-puddle-shell-init.sh\n".to_owned();
    host.store()
        .set_env(
            puddle_store::UserId::OWNER,
            &puddle_store::EnvScope::Workspace(name("acme")),
            &puddle_store::EnvName::new("EDITOR").unwrap(),
            puddle_store::EnvDraft::plain("vim").unwrap(),
        )
        .unwrap();
    rig.guest.boots_reach(booted + 1).await;
    events
        .until(
            is_event("workspace_shell_init_restored", "acme"),
            Duration::from_secs(20),
        )
        .await;
    host.shutdown().await;
}
