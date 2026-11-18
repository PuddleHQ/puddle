// SPDX-License-Identifier: AGPL-3.0-or-later
//! Failed starts, company roots and git settings that stop one.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn a_start_that_fails_after_the_ca_was_made_leaves_no_ca_behind() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    // A create whose boot fails.
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(host.workspaces().termination(&name("acme")).is_none());

    // A start in place whose boot fails.
    rig.guest.fail_boot.store(false, Ordering::SeqCst);
    create(&api, &mut events, "beta").await;
    api.post("/api/workspaces/beta/stop", "").await;
    events.until(ended("beta"), Duration::from_secs(20)).await;
    rig.guest.fail_boot.store(true, Ordering::SeqCst);
    api.post("/api/workspaces/beta/start", "").await;
    let end = events.until(ended("beta"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(host.workspaces().termination(&name("beta")).is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_the_runtime_refuses_raises_a_problem_about_the_workspace_until_it_starts() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "beta").await;
    api.post("/api/workspaces/beta/stop", "").await;
    events.until(ended("beta"), Duration::from_secs(20)).await;
    rig.runtime.inject(
        Op::Start,
        Fault::once(puddle_compute::ComputeError::StartRefused {
            sandbox: name("beta").sandbox_name().to_string(),
            cause: puddle_compute::StartRefusal::RootDiskNeedsRecovery,
        }),
    );
    api.post("/api/workspaces/beta/start", "").await;
    let end = events.until(ended("beta"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let problem = problem_of(&host, "start-refused:beta");
    assert_eq!(problem.workspace, Some(name("beta")));
    assert!(
        problem.title.contains("beta was not started"),
        "{problem:?}"
    );
    for want in [
        "nothing was attached",
        "the runtime cannot verify its stored root disk after an upgrade",
        "create it again",
    ] {
        assert!(problem.detail.contains(want), "{problem:?}");
    }

    api.post("/api/workspaces/beta/start", "").await;
    let end = events.until(ended("beta"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert!(!has_problem(&host, "start-refused:beta"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_company_roots_go_into_the_clients_that_verify_decrypted_hosts() {
    // A host with company roots starts: the TLS client for decrypted hosts is built with them.
    let rig = Rig::named_ids();
    let mut platform = FakePlatform::new(&rig.log);
    platform.roots = company_roots();
    let prepared = prepare(rig.config(), &platform).unwrap();
    let host = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        named_id_options(),
    )
    .await
    .unwrap();
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // The host's own TLS client took the company root: nothing is reported as left out.
    let report = api.get("/api/network-health").await.json();
    assert_eq!(report["roots"]["left_out_of_tls"], json!([]));
    // The guest got the company root and the CA in one extra-CAs file.
    let files = rig.guest.plan_files(0);
    let extra = pem_of(&files, "/etc/puddle/extra-cas.pem");
    assert_eq!(extra.matches("BEGIN CERTIFICATE").count(), 2, "{extra}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_company_root_the_hosts_own_tls_client_cannot_use_is_reported_as_left_out() {
    // The root states its extended key usage twice: the guest still gets it (its own verifiers
    // decide), but the host's TLS client for decrypted hosts leaves it out.
    let rig = Rig::new();
    let mut platform = FakePlatform::new(&rig.log);
    platform.roots = company_roots_with(|params| {
        for _ in 0..2 {
            params
                .custom_extensions
                .push(rcgen::CustomExtension::from_oid_content(
                    &[2, 5, 29, 37],
                    vec![48, 0],
                ));
        }
    });
    let fingerprint = platform.roots.certificates()[0].fingerprint().to_string();
    let prepared = prepare(rig.config(), &platform).unwrap();
    let host = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap();
    let report = api(&host).get("/api/network-health").await.json();
    let left_out = &report["roots"]["left_out_of_tls"];
    assert_eq!(left_out.as_array().map(Vec::len), Some(1), "{report}");
    assert_eq!(left_out[0]["subject"], "Corp Root CA");
    assert_eq!(left_out[0]["fingerprint"], fingerprint.as_str());
    assert_ne!(left_out[0]["reason"], "", "{left_out}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn git_settings_the_host_cannot_read_stop_a_start_and_change_nothing_while_running() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // acme's settings are damaged: its start says why and leaves no CA.
    damage_database(&rig, &UNREADABLE_GIT_ROW.replace("{}", "acme"));
    api.post("/api/workspaces/acme/start", "").await;
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(
        end["detail"]
            .as_str()
            .unwrap()
            .contains("cannot read acme's Git settings"),
        "{end}"
    );
    assert!(host.workspaces().termination(&name("acme")).is_none());

    // beta runs; its settings are damaged after the start: what it decrypts stays as it was.
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    attach(&api, "beta", identity).await;
    eventually("github.com decrypted", APPLY, || {
        decrypts(&host, "beta", "github.com")
    })
    .await;
    damage_database(&rig, &UNREADABLE_GIT_ROW.replace("{}", "beta"));
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("beta"),
    });
    // The next change is applied after the damaged one was looked at (events are handled in
    // order), and beta's set did not lose what it had.
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("beta"),
    });
    // The user is told: the old set stays and the next change tries again.
    eventually("the failed update is a problem", APPLY, || {
        has_problem(&host, "git-decrypt:beta")
    })
    .await;
    let problem = problem_of(&host, "git-decrypt:beta");
    assert_eq!(problem.title, "Puddle could not update what beta decrypts");
    assert!(
        problem.detail.contains("cannot read beta's Git settings")
            && problem.detail.contains("keeps the credentials it had"),
        "{}",
        problem.detail
    );
    assert!(decrypts(&host, "beta", "github.com"));

    // Once the settings read again, the next change takes the problem away.
    damage_database(
        &rig,
        "DELETE FROM workspace_repos WHERE repo = 'not a repo'",
    );
    host.events().emit(Event::WorkspaceGitChanged {
        workspace: name("beta"),
    });
    eventually("the problem ends", APPLY, || {
        !has_problem(&host, "git-decrypt:beta")
    })
    .await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rule_on_method_and_path_reaches_the_running_workspaces_and_decrypts_its_host() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert!(!decrypts(&host, "acme", "api.example.com"));

    // A rule for every workspace: the running one decrypts the host from then on.
    let made = api
        .post(
            "/api/request-rules",
            r#"{"workspace": null, "host": "api.example.com", "methods": ["DELETE"], "path": "/repos/**"}"#,
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.body);
    eventually("the rule's host is decrypted", APPLY, || {
        decrypts(&host, "acme", "api.example.com")
    })
    .await;

    // Deleting it takes the host out of the set again.
    let gone = api.delete("/api/request-rules/1", None).await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    eventually("the host is no longer decrypted", APPLY, || {
        !decrypts(&host, "acme", "api.example.com")
    })
    .await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_the_boot_plan_cannot_take_fails_the_start_and_leaves_nothing_behind() {
    let rig = Rig::named_ids();
    let odd = ImageRef::new("example.org/odd:1").unwrap();
    rig.runtime.add_image(
        &odd,
        ImageConfig {
            // A newline cannot be written into the guest's environment file.
            env: vec![("PATH".to_owned(), "/bin\n/usr/bin".to_owned())],
            ..ImageConfig::default()
        },
    );
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .post(
            "/api/workspaces",
            &json!({"name": "acme", "repositories": [{"url": REPO}], "image": "example.org/odd:1"})
                .to_string(),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    assert!(host.workspaces().termination(&name("acme")).is_none());
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_git_over_ssh_each_start_lists_its_own_host_keys_under_the_git_hosts() {
    let rig = Rig::named_ids();
    rig.git_ssh.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let first = rig.guest.known_hosts_block(rig.guest.boots() - 1).unwrap();
    // Three key types under the built-in names, each name plain and as `[name]:443`.
    for kind in ["ssh-ed25519", "ecdsa-sha2-nistp256", "ssh-rsa"] {
        for pattern in [
            "github.com",
            "[ssh.github.com]:443",
            "vs-ssh.visualstudio.com",
        ] {
            assert!(
                first.contains(&format!("{pattern} {kind} ")),
                "{pattern} {kind}"
            );
        }
    }
    // The block is a merge into the system file; nothing writes a file of Puddle's own, and the
    // private halves never reach the plan.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!files.contains_key("/etc/ssh/ssh_known_hosts"));
    assert!(
        files
            .values()
            .all(|bytes| !String::from_utf8_lossy(bytes).contains("PRIVATE KEY"))
    );

    // A restart makes new keys: the next plan lists them, and none of the old ones.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let second = rig.guest.known_hosts_block(rig.guest.boots() - 1).unwrap();
    let key_of = |block: &str| {
        block
            .lines()
            .next()
            .unwrap()
            .split(' ')
            .nth(2)
            .unwrap()
            .to_owned()
    };
    assert_ne!(key_of(&first), key_of(&second));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_git_over_ssh_an_identity_on_another_host_adds_that_name_and_keeps_the_keys() {
    let rig = Rig::named_ids();
    rig.git_ssh.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    let before = rig.guest.known_hosts_block(booted - 1).unwrap();
    assert!(!before.contains("git.corp.example"));

    let identity = make_identity(&api, "Corp", "git.corp.example", &[]).await;
    attach(&api, "acme", identity).await;
    rig.guest.boots_reach(booted + 1).await;
    let after = rig.guest.known_hosts_block(rig.guest.boots() - 1).unwrap();
    // The new name is listed in both spellings and every line that was there is still there.
    assert!(
        after.contains("\ngit.corp.example ssh-ed25519 ")
            || after.starts_with("git.corp.example ssh-ed25519 "),
        "{after}"
    );
    assert!(after.contains("[git.corp.example]:443 ssh-rsa "), "{after}");
    for line in before.lines() {
        assert!(after.lines().any(|l| l == line), "{line}");
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_git_over_ssh_the_guest_boots_with_no_host_keys() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    assert_eq!(rig.guest.known_hosts_block(rig.guest.boots() - 1), None);
    host.shutdown().await;
}
