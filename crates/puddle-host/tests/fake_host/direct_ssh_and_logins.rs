// SPDX-License-Identifier: AGPL-3.0-or-later
//! Direct SSH endpoints and entries, and login capture.

use super::*;

/// A session the runtime cannot serve ends the client's `ssh` with nothing to read, so the user
/// is told through the event stream.
#[tokio::test(flavor = "multi_thread")]
async fn an_ssh_session_the_runtime_cannot_serve_reaches_the_user_as_an_event() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    let name = WorkspaceId::new("acme").unwrap();
    let endpoint: PathBuf = host.workspaces().ssh_endpoint(&name).await.unwrap();
    rig.runtime.inject(
        Op::ServeSsh,
        Fault::once(puddle_compute::ComputeError::Runtime {
            op: "serve ssh",
            message: "the guest's sshd is gone".into(),
        }),
    );

    let (_stdin, bridge_in) = tokio::io::duplex(1024);
    let (bridge_out, _stdout) = tokio::io::duplex(1024);
    let _ = puddle_ssh::bridge::run(&endpoint, bridge_in, bridge_out).await;

    let event = events
        .until(
            |e| e["type"] == "workspace_ssh_failed",
            Duration::from_secs(20),
        )
        .await;
    assert_eq!(event["workspace"], "acme");
    let reason = event["reason"].as_str().unwrap();
    assert!(reason.contains("the guest's sshd is gone"), "{reason}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_direct_ssh_off_there_is_no_ssh_endpoint_and_the_gate_says_no() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let name = WorkspaceId::new("acme").unwrap();
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["status"],
        "running"
    );
    assert!(
        host.workspaces().ssh_endpoint(&name).await.is_none(),
        "no endpoint while the switch is off"
    );
    assert!(
        !host.workspaces().direct_ssh_allowed(&name),
        "the gate says no, so nothing else may open a way in"
    );
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["direct_ssh"],
        false
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_attach_with_unreadable_settings_says_so_instead_of_blaming_the_switch() {
    let rig = Rig::named_ids();
    // The settings are read once and kept, so the damage is there from the start; the workspace
    // names its memory, so the damaged file does not refuse the create.
    damage_global_settings(&rig);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let body = json!({"name": "acme", "repositories": [{"url": REPO}], "memory_mib": 4096});
    assert_eq!(
        api.post("/api/workspaces", &body.to_string()).await.status,
        202
    );
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let attached = host
        .workspaces()
        .attach(
            &puddle_services::owned::OwnedWorkspace::for_test(
                puddle_store::UserId::OWNER,
                &WorkspaceId::new("acme").unwrap(),
            ),
            puddle_services::workspaces::AttachMode::Desktop,
        )
        .await
        .unwrap();
    assert!(!attached.opened);
    let message = attached.message.unwrap();
    assert!(message.contains("settings cannot be read"), "{message}");
    assert!(!message.contains("allow it first"), "{message}");
    assert!(rig.launcher.opened.lock().unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_direct_ssh_on_and_off_opens_and_closes_the_endpoint_at_once() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let name = WorkspaceId::new("acme").unwrap();

    allow_direct_ssh(&api, "acme", true).await;
    assert!(host.workspaces().direct_ssh_allowed(&name));
    host.workspaces()
        .ssh_endpoint(&name)
        .await
        .expect("endpoint opens live");
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["direct_ssh"],
        true
    );

    allow_direct_ssh(&api, "acme", false).await;
    assert!(
        host.workspaces().ssh_endpoint(&name).await.is_none(),
        "endpoint closes live"
    );
    assert_eq!(
        api.get("/api/workspaces/acme").await.json()["direct_ssh"],
        false
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn login_capture_is_on_by_default_and_a_workspace_can_turn_it_off_for_its_next_start() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    // On by default: both decrypt the hosts of the Claude Code and GitHub logins and read their
    // token exchanges.
    for workspace in ["acme", "beta"] {
        for site in [
            "platform.claude.com",
            "api.anthropic.com",
            "github.com",
            "api.github.com",
        ] {
            assert!(decrypts(&host, workspace, site), "{workspace} {site}");
        }
        assert!(
            host.workspaces()
                .termination(&name(workspace))
                .unwrap()
                .exchanges()
                .is_some()
        );
    }

    // acme turns it off: it applies at its next start, and beta is not touched.
    let reply = api
        .put(
            "/api/settings/workspaces/acme",
            &json!({"overrides": {"capture_logins": false}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert!(
        decrypts(&host, "acme", "platform.claude.com"),
        "applies at the next start"
    );
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    for site in ["platform.claude.com", "api.anthropic.com", "api.github.com"] {
        assert!(!decrypts(&host, "acme", site), "{site}");
    }
    assert!(decrypts(&host, "beta", "platform.claude.com"));

    // The global default decides for a workspace with no switch of its own.
    let reply = api
        .put(
            "/api/settings",
            &json!({"workspace_defaults": {"capture_logins": false}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    api.post("/api/workspaces/beta/stop", "").await;
    events.until(ended("beta"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/beta/start", "").await;
    events.until(ended("beta"), Duration::from_secs(20)).await;
    assert!(!decrypts(&host, "beta", "platform.claude.com"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_kept_login_is_forgotten_through_the_api_by_the_services_short_name() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // Nothing is kept, which is fine to forget again; the service must be one Puddle has.
    let gone = api.delete("/api/workspaces/acme/logins/claude", None).await;
    assert_eq!(gone.status, 204, "{}", gone.body);
    // A credential store that refuses the deletion is a 503, and works again once it answers.
    rig.secrets.break_it();
    let refused = api.delete("/api/workspaces/acme/logins/claude", None).await;
    assert_eq!(refused.status, 503, "{}", refused.body);
    assert!(refused.body.contains("ask again"), "{}", refused.body);
    rig.secrets.heal();
    let again = api.delete("/api/workspaces/acme/logins/claude", None).await;
    assert_eq!(again.status, 204, "{}", again.body);
    let unknown = api.delete("/api/workspaces/acme/logins/azure", None).await;
    assert_eq!(unknown.status, 404, "{}", unknown.body);
    let no_workspace = api
        .delete("/api/workspaces/nobody/logins/claude", None)
        .await;
    assert_eq!(no_workspace.status, 404, "{}", no_workspace.body);
    host.shutdown().await;
}

/// The saved logins of a workspace outlive a restart of Puddle, whose start-up clean-up removes
/// what nothing names: those of a workspace that is gone go, those of one that is still there stay.
#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_the_saved_logins_of_every_workspace_and_drops_those_of_one_that_is_gone() {
    let rig = Rig::named_ids();
    rig.indexed.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    host.shutdown().await;
    drop(host);
    // What the run before captured, written the way a run writes: through the index.
    let earlier = puddle_secrets::IndexedStore::new(SharedSecrets(rig.secrets.clone()));
    let kept = puddle_logins::saved_login_ids(
        &WorkspaceId::new("acme").unwrap(),
        puddle_logins::builtin(),
    );
    let gone = puddle_logins::saved_login_ids(
        &WorkspaceId::new("gone").unwrap(),
        puddle_logins::builtin(),
    );
    for id in kept.iter().chain(&gone) {
        earlier
            .set(id, &puddle_secrets::Secret::new("{}".into()))
            .unwrap();
    }
    // The clean-up runs at every start, and a quit waits for it.
    for _ in 0..2 {
        let host = rig.start().await;
        host.shutdown().await;
    }
    for id in &kept {
        assert!(
            rig.secrets.get(id).unwrap().is_some(),
            "{id:?} of acme was removed"
        );
    }
    for id in &gone {
        assert!(
            rig.secrets.get(id).unwrap().is_none(),
            "{id:?} of a gone workspace stayed"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_kept_logins_list_says_nothing_is_kept_and_does_not_wait_on_the_credential_store() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let listed = api.get("/api/workspaces/acme/logins").await;
    assert_eq!(listed.status, 200, "{}", listed.body);
    let listed: serde_json::Value = serde_json::from_str(&listed.body).unwrap();
    assert_eq!(listed, json!({"logins": [], "store_readable": true}));
    // Puddle's own record answers now, so a store that is locked or gone does not stall the list.
    rig.secrets.break_it();
    let down = api.get("/api/workspaces/acme/logins").await;
    assert_eq!(down.status, 200, "{}", down.body);
    let down: serde_json::Value = serde_json::from_str(&down.body).unwrap();
    assert_eq!(down, json!({"logins": [], "store_readable": true}));
    let no_workspace = api.get("/api/workspaces/nobody/logins").await;
    assert_eq!(no_workspace.status, 404, "{}", no_workspace.body);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_global_default_decides_for_workspaces_without_their_own_switch() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .put(
            "/api/settings",
            &json!({"workspace_defaults": {"direct_ssh": true}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    create(&api, &mut events, "acme").await;
    let name = WorkspaceId::new("acme").unwrap();
    assert!(host.workspaces().ssh_endpoint(&name).await.is_some());
    // Its own switch wins over the default.
    allow_direct_ssh(&api, "acme", false).await;
    assert!(host.workspaces().ssh_endpoint(&name).await.is_none());
    host.shutdown().await;
}

/// The line Puddle adds to the user's ssh config. On Windows the product writes the path with
/// forward slashes (OpenSSH accepts them, and a backslash is an escape for some parsers).
fn include_line(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    };
    format!("Include \"{text}\"")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_runtime_gets_the_login_key_and_the_ssh_entry_follows_the_endpoint() {
    let rig = Rig::named_ids();
    let factory = FakeFactory::new(&rig.runtime, &rig.log);
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let mut options = named_id_options();
    options.secret_store = Some(rig.secrets.clone());
    options.dev_cert = Some(rig.dev_cert.clone());
    let host = Host::start(prepared, &factory, options).await.unwrap();
    let key = factory.ssh_login_key.lock().unwrap().clone().unwrap();
    let public =
        std::fs::read_to_string(rig.dir.path().join("data/ssh/login_ed25519.pub")).unwrap();
    assert_eq!(key, public.trim());
    assert!(key.starts_with("ssh-ed25519 "));

    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceId::new("acme").unwrap();
    // Off: no entry, and the user's own file is not touched.
    create(&api, &mut events, "acme").await;
    assert!(ssh_hosts(&rig).is_empty());
    assert!(!rig.user_ssh_config().exists());

    // On: the entry names the endpoint, and the user's file gets its one line.
    allow_direct_ssh(&api, "acme", true).await;
    let first = host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    let entry = std::fs::read_to_string(rig.puddle_ssh_config()).unwrap();
    assert!(entry.contains(&first.display().to_string()), "{entry}");
    assert!(entry.contains("ssh-bridge"), "{entry}");
    let include = include_line(&rig.puddle_ssh_config());
    let user = std::fs::read_to_string(rig.user_ssh_config()).unwrap();
    assert_eq!(user.lines().next(), Some(include.as_str()), "{user}");

    // Off again: the entry goes with the endpoint, the include line stays (and is not doubled).
    allow_direct_ssh(&api, "acme", false).await;
    assert!(ssh_hosts(&rig).is_empty());
    allow_direct_ssh(&api, "acme", true).await;
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    assert_eq!(
        std::fs::read_to_string(rig.user_ssh_config()).unwrap(),
        user
    );

    // A stop takes the entry out; the next start writes the new endpoint (the name is random per run).
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(ssh_hosts(&rig).is_empty());
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let second = host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_ne!(first, second);
    let entry = std::fs::read_to_string(rig.puddle_ssh_config()).unwrap();
    assert!(
        entry.contains(&second.display().to_string())
            && !entry.contains(&first.display().to_string())
    );

    // Shutting down leaves no entry.
    host.shutdown().await;
    assert!(ssh_hosts(&rig).is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_follows_the_global_default_gets_its_entry_and_one_that_overrides_it_does_not()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let reply = api
        .put(
            "/api/settings",
            &json!({"workspace_defaults": {"direct_ssh": true}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    create(&api, &mut events, "acme").await;
    create_with_direct_ssh(&api, &mut events, "beta", false).await;
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_left_by_a_crashed_run_is_gone_when_the_host_starts() {
    let rig = Rig::new();
    let stale = rig.puddle_ssh_config();
    std::fs::create_dir_all(stale.parent().unwrap()).unwrap();
    std::fs::write(&stale, "Host puddle-ghost\n    User root\n").unwrap();
    let host = rig.start().await;
    assert!(ssh_hosts(&rig).is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_that_cannot_be_written_is_a_problem_that_ends_when_it_can() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // The user's ssh config is a folder: the include line cannot be added.
    std::fs::create_dir_all(rig.user_ssh_config()).unwrap();
    allow_direct_ssh(&api, "acme", true).await;
    let (title, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(title.contains("acme"), "{title}");
    assert!(detail.contains("include line"), "{detail}");
    // The attach says it too, instead of opening an editor that cannot connect.
    let attach = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(attach.json()["opened"], false, "{}", attach.body);
    assert!(
        attach.json()["message"]
            .as_str()
            .unwrap()
            .contains("include line")
    );
    assert!(rig.launcher.opened.lock().unwrap().is_empty());
    // Fixed: the next attach writes it, opens the editor and the problem is gone.
    std::fs::remove_dir(rig.user_ssh_config()).unwrap();
    let attach = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(attach.json()["opened"], true, "{}", attach.body);
    assert!(problem_is(&api, "ssh:acme", false).await.is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_entry_that_cannot_be_taken_out_is_a_problem_until_the_file_can_be_written() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    allow_direct_ssh(&api, "acme", true).await;
    eventually("the entry", APPLY, || {
        std::fs::read_to_string(rig.puddle_ssh_config()).is_ok_and(|t| t.contains("puddle-acme"))
    })
    .await;
    // A folder with a file in it where Puddle's file is: the file cannot be rewritten.
    std::fs::remove_file(rig.puddle_ssh_config()).unwrap();
    std::fs::create_dir_all(rig.puddle_ssh_config().join("in")).unwrap();
    allow_direct_ssh(&api, "acme", false).await;
    let (title, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(title.contains("still names acme"), "{title}");
    assert!(detail.contains("cannot take puddle-acme"), "{detail}");
    // Fixed: the next entry written for it ends the problem.
    std::fs::remove_dir_all(rig.puddle_ssh_config()).unwrap();
    allow_direct_ssh(&api, "acme", true).await;
    assert!(problem_is(&api, "ssh:acme", false).await.is_none());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_problem_with_direct_ssh_ends_when_the_switch_goes_off_or_the_workspace_stops() {
    for ending in ["off", "stop"] {
        let rig = Rig::named_ids();
        let host = rig.start().await;
        let api = api(&host);
        let mut events = api.events().await;
        create(&api, &mut events, "acme").await;
        std::fs::create_dir_all(rig.user_ssh_config()).unwrap();
        allow_direct_ssh(&api, "acme", true).await;
        problem_is(&api, "ssh:acme", true).await.unwrap();
        if ending == "off" {
            allow_direct_ssh(&api, "acme", false).await;
        } else {
            api.post("/api/workspaces/acme/stop", "").await;
            events.until(ended("acme"), Duration::from_secs(20)).await;
        }
        assert!(
            problem_is(&api, "ssh:acme", false).await.is_none(),
            "{ending}"
        );
        host.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_the_puddle_program_says_why_direct_ssh_has_no_entry() {
    let rig = Rig::named_ids();
    let mut config = rig.config();
    config.ssh.bridge_program = None;
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    let (_, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(detail.contains("no `puddle` program"), "{detail}");
    assert!(ssh_hosts(&rig).is_empty());
    host.shutdown().await;
}
