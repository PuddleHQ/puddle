// SPDX-License-Identifier: AGPL-3.0-or-later
//! Changes that reach a booting or running guest, and merge notices.

use super::*;

/// Makes the next run of the boot hook attach a new identity to `workspace`, as a user clicking
/// while the guest boots would.
fn attach_during_the_next_boot(rig: &Rig, host: &Host<FakeRuntime>, workspace: &str) {
    let store = host.store().clone();
    let workspace = name(workspace);
    *rig.guest.during_boot.lock().unwrap() =
        Some(Box::new(move || attach_a_new_identity(&store, &workspace)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_made_while_the_guest_boots_reaches_it_once_it_is_up() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    // No identity exists when the workspace is created, so its plan names no author.
    attach_during_the_next_boot(&rig, &host, "acme");
    create(&api, &mut events, "acme").await;

    // The set followed while the guest was booting, and once it was up the guest was brought in
    // line with a second run of the hook.
    assert!(decrypts(&host, "acme", "github.com"));
    rig.guest.boots_reach(2).await;
    let first = rig.guest.plan_files(0);
    assert!(!pem_of(&first, "/etc/puddle/gitconfig").contains("[user]"));
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_change_made_while_a_stopped_workspace_starts_again_reaches_it_once_it_is_up() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let booted = rig.guest.boots();

    attach_during_the_next_boot(&rig, &host, "acme");
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(decrypts(&host, "acme", "github.com"));
    rig.guest.boots_reach(booted + 2).await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_that_turn_unreadable_during_a_boot_do_not_fail_the_boot() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let db = rig.dir.path().join("data").join("puddle.db");
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        let db = rusqlite::Connection::open(db).unwrap();
        db.execute_batch(&UNREADABLE_GIT_ROW.replace("{}", "acme"))
            .unwrap();
    }));
    // The boot had what it needed; the workspace comes up with the settings it started with.
    create(&api, &mut events, "acme").await;
    assert!(host.workspaces().termination(&name("acme")).is_some());
    assert_eq!(rig.guest.boots(), 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_guest_that_cannot_take_new_authors_keeps_the_old_ones_and_the_next_change_retries() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let ada = make_identity(&api, "Ada", "github.com", &[]).await;
    let bob = make_identity(&api, "Bob", "gitlab.com", &[]).await;

    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    attach(&api, "acme", ada).await;
    rig.guest.boots_reach(booted + 1).await;
    // What is decrypted followed anyway: only the guest's files could not be written.
    eventually("github.com decrypted", APPLY, || {
        decrypts(&host, "acme", "github.com")
    })
    .await;
    // The user is told, with the guest's reason.
    eventually("the failed rewrite is a problem", APPLY, || {
        has_problem(&host, "git-authors:acme")
    })
    .await;
    let problem = problem_of(&host, "git-authors:acme");
    assert_eq!(
        problem.title,
        "Puddle could not update the commit authors in acme"
    );
    assert!(
        problem.detail.contains("boot.sh: no disk"),
        "{}",
        problem.detail
    );

    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    attach(&api, "acme", bob).await;
    rig.guest.boots_reach(booted + 2).await;
    eventually("the retry takes the problem away", APPLY, || {
        !has_problem(&host, "git-authors:acme")
    })
    .await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let config = pem_of(&files, "/etc/puddle/gitconfig");
    // Both identities are in the retry: the first change was never written.
    assert!(config.contains("name = \"Ada\""), "{config}");
    author_file(&files, "Bob");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_that_go_unreadable_while_a_rewrite_waits_are_a_problem_the_next_change_ends() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let ada = make_identity(&api, "Ada", "github.com", &[]).await;
    let bob = make_identity(&api, "Bob", "gitlab.com", &[]).await;

    // Ada's rewrite holds the guest in its boot hook, so Bob's change reads what it decrypts and
    // then waits for the rewrite to end.
    let (release, hold) = std::sync::mpsc::channel::<()>();
    let hold = Mutex::new(hold);
    *rig.guest.during_boot.lock().unwrap() = Some(Box::new(move || {
        wait_for(&hold.lock().unwrap());
    }));
    attach(&api, "acme", ada).await;
    rig.guest.boots_reach(booted + 1).await;
    attach(&api, "acme", bob).await;
    eventually("gitlab.com decrypted", APPLY, || {
        decrypts(&host, "acme", "gitlab.com")
    })
    .await;

    // The settings go bad before Bob's rewrite can read them.
    damage_database(&rig, &UNREADABLE_GIT_ROW.replace("{}", "acme"));
    release.send(()).unwrap();
    eventually("the failed rewrite is a problem", APPLY, || {
        has_problem(&host, "git-authors:acme")
    })
    .await;
    let problem = problem_of(&host, "git-authors:acme");
    assert_eq!(
        problem.title,
        "Puddle could not update the commit authors in acme"
    );
    assert!(
        problem.detail.contains("cannot read acme's Git settings")
            && problem
                .detail
                .contains("the next change to its identities tries again"),
        "{}",
        problem.detail
    );

    // Once the settings read again, the next change writes both authors and ends the problem.
    damage_database(
        &rig,
        "DELETE FROM workspace_repos WHERE repo = 'not a repo'",
    );
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("acme"),
    });
    eventually("the retry takes the problem away", APPLY, || {
        !has_problem(&host, "git-authors:acme")
    })
    .await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    author_file(&files, "Bob");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_git_too_old_for_the_author_rules_is_a_problem_until_it_is_updated() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let ada = make_identity(&api, "Ada", "github.com", &["acme"]).await;
    let bob = make_identity(&api, "Bob", "gitlab.com", &["labs"]).await;

    // The guest's git is from before `includeIf hasconfig`: the rules are written and ignored.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.34.1\n".to_owned();
    attach(&api, "acme", ada).await;
    rig.guest.boots_reach(booted + 1).await;
    eventually("the old git is a problem", APPLY, || {
        has_problem(&host, "git-old:acme")
    })
    .await;
    let problem = problem_of(&host, "git-old:acme");
    assert_eq!(
        problem.title,
        "git 2.34 in acme ignores the commit author rules"
    );
    assert!(
        problem.detail.contains("2.36") && problem.detail.contains("first identity's author"),
        "{}",
        problem.detail
    );

    // A git that reads them, at the next rewrite, ends it.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.39.5\n".to_owned();
    attach(&api, "acme", bob).await;
    rig.guest.boots_reach(booted + 2).await;
    eventually("the problem ends", APPLY, || {
        !has_problem(&host, "git-old:acme")
    })
    .await;

    // And a stopped workspace has none.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.30.2\n".to_owned();
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!has_problem(&host, "git-old:acme"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn kubernetes_tools_are_a_problem_until_a_start_finds_none() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert!(!has_problem(&host, "kubernetes:acme"));

    // The start after kind and k3d were installed says so, and what to do.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: kubernetes tools kind k3d\n".to_owned();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let problem = problem_of(&host, "kubernetes:acme");
    assert_eq!(problem.title, "Kubernetes Services do not work in acme");
    assert!(
        problem.detail.contains("kind, k3d") && problem.detail.contains("port forward"),
        "{}",
        problem.detail
    );

    // A stopped workspace has none.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!has_problem(&host, "kubernetes:acme"));

    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_merged_file_puddle_cannot_parse_is_a_notice_until_a_start_finds_it_repaired() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert!(!has_problem(
        &host,
        "merge-left:acme:/home/agent/.docker/config.json"
    ));

    // A start that finds both files unparsable names each file, the error and what fails.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: /home/agent/.docker/config.json left as it is: \
        the file is not valid JSON: EOF while parsing at line 1 column 1\n\
        puddle-boot: /home/agent/.vscode-server/data/Machine/settings.json left as it is: \
        the file is not valid JSON: expected value at line 2 column 3\n"
            .to_owned();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let docker = problem_of(&host, "merge-left:acme:/home/agent/.docker/config.json");
    assert_eq!(
        docker.title,
        "Puddle could not update /home/agent/.docker/config.json in acme"
    );
    assert!(
        docker
            .detail
            .contains("EOF while parsing at line 1 column 1")
            && docker.detail.contains("bypasses the proxy")
            && docker.detail.contains("pulls"),
        "{}",
        docker.detail
    );
    let machine = problem_of(
        &host,
        "merge-left:acme:/home/agent/.vscode-server/data/Machine/settings.json",
    );
    assert!(
        machine.detail.contains("expected value at line 2 column 3")
            && machine.detail.contains("port forwarding"),
        "{}",
        machine.detail
    );

    // The user repaired the files: the next start says nothing, and the old notices are gone.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!has_problem(
        &host,
        "merge-left:acme:/home/agent/.docker/config.json"
    ));
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!has_problem(
        &host,
        "merge-left:acme:/home/agent/.docker/config.json"
    ));
    assert!(!has_problem(
        &host,
        "merge-left:acme:/home/agent/.vscode-server/data/Machine/settings.json"
    ));
    host.shutdown().await;
}

/// The boot hook runs in the guest, whose root writes what it prints: only the files Puddle's plan
/// merges can get a notice, each once, with the reason cut and its control characters replaced.
#[tokio::test(flavor = "multi_thread")]
async fn hostile_a_flood_of_left_files_is_at_most_one_cleaned_notice_per_merged_file() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let long = format!("bad\x1b[2J\rfaked{}", "x".repeat(10 * 1024));
    let mut says = String::new();
    for i in 0..1000 {
        says.push_str(&format!(
            "puddle-boot: /tmp/forged-{i} left as it is: {long}\n"
        ));
        says.push_str(&format!(
            "puddle-boot: /etc/ssh/ssh_config left as it is: {i} {long}\n"
        ));
    }
    *rig.guest.boot_says.lock().unwrap() = says;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let merges: Vec<_> = host
        .problems()
        .list()
        .into_iter()
        .filter(|p| p.key.starts_with("merge-left:acme:"))
        .collect();
    let keys: Vec<&str> = merges.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, ["merge-left:acme:/etc/ssh/ssh_config"]);
    let detail = &merges[0].detail;
    assert!(
        detail.contains(": 0 bad"),
        "the first line counts: {detail}"
    );
    assert!(!detail.chars().any(char::is_control), "{detail:?}");
    assert!(detail.len() < 1024, "{} bytes", detail.len());
    host.shutdown().await;
}

/// Waits until `has_problem(key)` is `want`; a plan rewrite in a running guest settles in the
/// background.
async fn problem_settles(host: &Host<FakeRuntime>, key: &str, want: bool) {
    eventually(
        &format!("{key} has problem is {want}"),
        Duration::from_secs(10),
        || has_problem(host, key) == want,
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_plan_rewrite_in_a_running_workspace_raises_and_ends_the_merge_notice() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let key = "merge-left:acme:/etc/ssh/ssh_config";
    assert!(!has_problem(&host, key));

    // A variable changed while it runs: the hook runs again, finds the file unreadable, and says so.
    *rig.guest.boot_says.lock().unwrap() =
        "puddle-boot: /etc/ssh/ssh_config left as it is: the file is not UTF-8\n".to_owned();
    put_env(
        &api,
        "/api/workspaces/acme/env/A_VAR",
        json!({"kind": "plain", "value": "1"}),
    )
    .await;
    problem_settles(&host, key, true).await;
    let problem = problem_of(&host, key);
    assert!(
        problem.detail.contains("not UTF-8") && problem.detail.contains("Connection closed"),
        "{}",
        problem.detail
    );

    // The next rewrite finds it repaired: the notice ends without a restart.
    put_env(
        &api,
        "/api/workspaces/acme/env/A_VAR",
        json!({"kind": "plain", "value": "2"}),
    )
    .await;
    problem_settles(&host, key, false).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_git_too_old_is_noticed_at_a_start_too_but_only_when_author_rules_exist() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    // No identity yet, so no rule for the old git to ignore.
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.30.2\n".to_owned();
    create(&api, &mut events, "acme").await;
    assert!(!has_problem(&host, "git-old:acme"));
    let ada = make_identity(&api, "Ada", "github.com", &["acme"]).await;

    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    attach(&api, "acme", ada).await;
    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.30.2\n".to_owned();
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(
        has_problem(&host, "git-old:acme"),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}

/// Every run of the boot hook in a running guest settles its report the same way, whatever
/// asked for the run: here a rewrite for a changed variable, not one for the authors.
#[tokio::test(flavor = "multi_thread")]
async fn a_plan_rewrite_for_a_variable_notices_an_old_git_too() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let ada = make_identity(&api, "Ada", "github.com", &["acme"]).await;
    attach(&api, "acme", ada).await;
    rig.guest.boots_reach(booted + 1).await;
    assert!(!has_problem(&host, "git-old:acme"));

    *rig.guest.boot_says.lock().unwrap() = "puddle-boot: git version 2.34.1\n".to_owned();
    put_env(
        &api,
        "/api/workspaces/acme/env/A_VAR",
        json!({"kind": "plain", "value": "1"}),
    )
    .await;
    eventually("the old git is a problem", APPLY, || {
        has_problem(&host, "git-old:acme")
    })
    .await;
    host.shutdown().await;
}

/// A binding the store would refuse today, as an older database holds it.
fn old_binding_on_port(host_name: &str) -> puddle_store::CredentialBinding {
    serde_json::from_value(json!({
        "host": host_name,
        "source": {"kind": "gh", "host": host_name, "account": "me"},
        "covers": {"owners": [], "rest_of_host": true}
    }))
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_for_a_port_puddle_never_decrypts_is_refused_and_an_old_one_is_a_problem() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;

    // Saving one is refused with the reason.
    let body = json!({
        "label": "Ada",
        "author": {"name": "Ada", "email": "ada@example.org"},
        "credentials": [{
            "host": "ghe.example:8443",
            "source": {"kind": "gh", "host": "ghe.example:8443", "account": "me"},
            "covers": {"owners": [], "rest_of_host": true}
        }]
    });
    let reply = api.post("/api/identities", &body.to_string()).await;
    assert_eq!(reply.status, 422, "{}", reply.body);
    assert!(
        reply.body.contains("port 8443") && reply.body.contains("never be used"),
        "{}",
        reply.body
    );

    // One a database from before already holds is said to be useless, while its workspace runs.
    let old = host
        .store()
        .create_identity(
            puddle_store::UserId::OWNER,
            puddle_store::IdentityDraft {
                label: "Old".to_owned(),
                author: puddle_store::Author::new("Old", "old@example.org").unwrap(),
                credentials: vec![old_binding_on_port("ghe.example:8443")],
            },
        )
        .unwrap();
    host.store()
        .attach_identity(&name("acme"), old.id, None)
        .unwrap();
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("acme"),
    });
    eventually("the useless credential is a problem", APPLY, || {
        has_problem(&host, "git-credentials:acme")
    })
    .await;
    let problem = problem_of(&host, "git-credentials:acme");
    assert_eq!(problem.title, "acme has credentials Puddle never uses");
    assert!(
        problem.detail.contains("ghe.example:8443"),
        "{}",
        problem.detail
    );
    assert!(!decrypts(&host, "acme", "ghe.example"));

    // Detached, it ends.
    host.store().detach_identity(&name("acme"), old.id).unwrap();
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("acme"),
    });
    eventually("the problem ends", APPLY, || {
        !has_problem(&host, "git-credentials:acme")
    })
    .await;
    host.shutdown().await;
}

/// Holds the next author rewrite that starts back at the host's gate until `release`, and says
/// when it has ended.
#[derive(Default)]
struct HeldRewrite {
    armed: AtomicBool,
    arrived: AtomicBool,
    ended: AtomicBool,
    release: tokio::sync::Notify,
}

/// Dropped when the rewrite the gate let on ends; says so if it was the held one.
struct RewriteEnd(Option<Arc<HeldRewrite>>);

impl Drop for RewriteEnd {
    fn drop(&mut self) {
        if let Some(held) = &self.0 {
            held.ended.store(true, Ordering::SeqCst);
        }
    }
}

impl HeldRewrite {
    fn gate(self: &Arc<Self>) -> RewriteGate {
        let held = self.clone();
        Arc::new(move |_| {
            let held = held.clone();
            Box::pin(async move {
                let this_one = held.armed.swap(false, Ordering::SeqCst);
                if this_one {
                    held.arrived.store(true, Ordering::SeqCst);
                    held.release.notified().await;
                }
                Box::new(RewriteEnd(this_one.then_some(held))) as Box<dyn Send>
            })
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_author_rewrite_that_runs_after_a_newer_change_applies_the_newest_settings() {
    let rig = Rig::named_ids();
    let held = Arc::new(HeldRewrite::default());
    *rig.rewrite_gate.lock().unwrap() = Some(held.gate());
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let old = host
        .store()
        .create_identity(
            puddle_store::UserId::OWNER,
            puddle_store::IdentityDraft {
                label: "Old".to_owned(),
                author: puddle_store::Author::new("Old", "old@example.org").unwrap(),
                credentials: vec![old_binding_on_port("ghe.example:8443")],
            },
        )
        .unwrap();

    // The store announces each change itself. The rewrite for the attach starts first and waits at the gate.
    held.armed.store(true, Ordering::SeqCst);
    host.store()
        .attach_identity(&name("acme"), old.id, None)
        .unwrap();
    eventually("the attach rewrite waits", APPLY, || {
        held.arrived.load(Ordering::SeqCst)
    })
    .await;
    // The detach comes after, and its rewrite runs through: nothing is left to rewrite or to say.
    host.store().detach_identity(&name("acme"), old.id).unwrap();
    eventually("the detach ends the problem", APPLY, || {
        !has_problem(&host, "git-credentials:acme")
    })
    .await;

    // The attach rewrite now goes on, last. It must apply the settings as they are now.
    held.release.notify_one();
    eventually("the attach rewrite ends", APPLY, || {
        held.ended.load(Ordering::SeqCst)
    })
    .await;
    assert!(
        !has_problem(&host, "git-credentials:acme"),
        "{:?}",
        host.problems().list()
    );
    assert_eq!(rig.guest.boots(), booted, "the old author was written back");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_author_rewrite_that_finds_its_workspace_stopped_in_the_meantime_writes_nothing() {
    let rig = Rig::named_ids();
    let held = Arc::new(HeldRewrite::default());
    *rig.rewrite_gate.lock().unwrap() = Some(held.gate());
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    let booted = rig.guest.boots();

    // The attach's rewrite starts while the workspace runs and waits at the gate; the workspace
    // is stopped before it looks at it.
    held.armed.store(true, Ordering::SeqCst);
    host.store()
        .attach_identity(&name("acme"), puddle_store::IdentityId(identity), None)
        .unwrap();
    eventually("the attach rewrite waits", APPLY, || {
        held.arrived.load(Ordering::SeqCst)
    })
    .await;
    stop_and_wait(&api, &mut events, "acme").await;
    held.release.notify_one();
    eventually("the attach rewrite ends", APPLY, || {
        held.ended.load(Ordering::SeqCst)
    })
    .await;
    assert_eq!(rig.guest.boots(), booted, "a stopped guest was written to");
    assert!(
        !has_problem(&host, "git-credentials:acme"),
        "{:?}",
        host.problems().list()
    );
    host.shutdown().await;
}
