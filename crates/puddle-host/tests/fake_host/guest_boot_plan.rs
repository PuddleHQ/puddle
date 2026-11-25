// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a start puts into the guest: CA, editor server, SSH agent, authors.

use super::*;

const KEY_MARKER: &[u8] = b"PRIVATE KEY";

#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspace_trusts_its_own_ca_from_the_first_boot_and_decrypts_only_the_login_hosts_yet()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;

    // The CA the guest was given is the one the proxy signs with, and each workspace has its own.
    let ca_of = |workspace: &str| {
        host.workspaces()
            .termination(&name(workspace))
            .unwrap()
            .ca()
            .certificate()
            .clone()
    };
    let (acme, beta) = (ca_of("acme"), ca_of("beta"));
    assert_ne!(acme, beta);
    let files = rig.guest.plan_files(0);
    for path in [
        "/etc/puddle/extra-cas.pem",
        "/usr/local/share/ca-certificates/puddle/puddle-ca-1.crt",
    ] {
        assert_eq!(pem_of(&files, path), acme.pem(), "{path}");
    }
    let guest_der =
        rustls::pki_types::CertificateDer::from_pem_slice(&files["/etc/puddle/extra-cas.pem"])
            .unwrap();
    assert_eq!(guest_der.as_ref(), acme.der().as_ref());
    assert_ne!(
        pem_of(
            &rig.guest.plan_files(rig.guest.boots() - 1),
            "/etc/puddle/extra-cas.pem"
        ),
        acme.pem()
    );
    // Tools are pointed at the bundle, though no identity decrypts anything yet.
    let env = pem_of(&files, "/etc/profile.d/01-puddle-env.sh");
    assert!(
        env.contains("export SSL_CERT_FILE='/etc/puddle/ca-bundle.pem'"),
        "{env}"
    );
    assert!(
        env.contains("export NODE_EXTRA_CA_CERTS='/etc/puddle/extra-cas.pem'"),
        "{env}"
    );
    // Login capture is on by default: the hosts of the logins are decrypted from the start, and
    // no other.
    assert!(decrypts(&host, "acme", "platform.claude.com"));
    assert!(!decrypts(&host, "acme", "gitlab.com"));
    // No key anywhere in what the guest was sent.
    for (_, plan) in rig.guest.plans.lock().unwrap().iter() {
        assert!(!plan.windows(KEY_MARKER.len()).any(|w| w == KEY_MARKER));
        assert!(!String::from_utf8_lossy(plan).contains("PRIVATE"));
    }
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_credential_for_any_host_applies_to_the_running_workspace_with_no_restart() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let ca_before = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    let (creates, starts, stops) = (
        calls_of(&rig, "Create"),
        calls_of(&rig, "Start"),
        calls_of(&rig, "Stop"),
    );

    // A host the workspace has never used, on a host Puddle's default list does not have.
    let gitlab = make_identity(&api, "Ada", "gitlab.com", &[]).await;
    attach(&api, "acme", gitlab).await;
    eventually("gitlab.com decrypted", APPLY, || {
        decrypts(&host, "acme", "gitlab.com")
    })
    .await;
    // A second one, on Azure DevOps: both of its names.
    let azure = make_identity(&api, "Bob", "dev.azure.com", &["contoso"]).await;
    attach(&api, "acme", azure).await;
    eventually("azure decrypted", APPLY, || {
        decrypts(&host, "acme", "contoso.visualstudio.com")
    })
    .await;
    assert!(decrypts(&host, "acme", "dev.azure.com"));
    // Nothing else is: a host nobody gave a credential, and that no login is for, is still spliced.
    assert!(!decrypts(&host, "acme", "example.com"));
    assert!(!decrypts(&host, "acme", "api.gitlab.com"));
    // The CA is the one the guest booted with, and the sandbox was not touched.
    let after = host.workspaces().termination(&name("acme")).unwrap();
    assert_eq!(after.ca().certificate(), &ca_before);
    assert_eq!(
        (
            calls_of(&rig, "Create"),
            calls_of(&rig, "Start"),
            calls_of(&rig, "Stop")
        ),
        (creates, starts, stops)
    );

    // Taking an identity off takes its hosts out of the set on the next connection.
    let gone = api
        .delete(&format!("/api/workspaces/acme/identities/{gitlab}"), None)
        .await;
    assert_eq!(gone.status, 200, "{}", gone.body);
    eventually("gitlab.com no longer decrypted", APPLY, || {
        !decrypts(&host, "acme", "gitlab.com")
    })
    .await;
    assert!(decrypts(&host, "acme", "dev.azure.com"));
    host.shutdown().await;
}

/// A folder that looks like an unpacked code-server release, for `GuestSettings::code_server`.
fn fake_code_server(rig: &Rig) -> PathBuf {
    let dir = rig.dir.path().join("code-server-release");
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::write(dir.join("bin/code-server"), b"#!/bin/sh\n").unwrap();
    std::fs::write(dir.join("code-server.json"), br#"{"version": "9.9.9"}"#).unwrap();
    dir
}

fn agent_settings(plan: &[u8]) -> std::collections::BTreeMap<String, String> {
    String::from_utf8_lossy(plan)
        .lines()
        .filter_map(|l| l.strip_prefix("puddle_agent_env "))
        .filter_map(|l| {
            let (name, value) = l.split_once(' ')?;
            Some((
                name.trim_matches('\'').to_owned(),
                value.trim_matches('\'').to_owned(),
            ))
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_with_code_server_tells_each_guest_where_to_run_the_editor_server() {
    let rig = Rig::named_ids();
    let mut config = rig.config();
    config.guest.code_server = Some(fake_code_server(&rig));
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let installed = rig.dir.path().join("data/guest-share/puddle/code-server");
    assert!(installed.join("bin/code-server").is_file());
    assert!(installed.join(".puddle-installed").is_file());
    let seen = agent_settings(&rig.guest.plan_bytes(rig.guest.boots() - 1));
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_DIR").map(String::as_str),
        Some("/puddle/code-server")
    );
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_DATA").map(String::as_str),
        Some("/workspace/.puddle/code-server")
    );
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_FOLDER").map(String::as_str),
        Some("/workspace")
    );
    // Where an attach writes the multi-root file, for a server that starts after one.
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_WORKSPACE_FILE")
            .map(String::as_str),
        Some("/workspace/.puddle/editor/acme.code-workspace")
    );
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_GRACE").map(String::as_str),
        Some("300")
    );
    // The server runs as the session user of this start: agent for a new workspace, and after a
    // switch to root, root (no variable) from the next start.
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_USER").map(String::as_str),
        Some("agent")
    );
    let put = api
        .put(
            "/api/workspaces/acme/session-user",
            &json!({"session_user": "root"}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    for step in ["stop", "start"] {
        let reply = api.post(&format!("/api/workspaces/acme/{step}"), "").await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        events.until(ended("acme"), Duration::from_secs(20)).await;
    }
    let seen = agent_settings(&rig.guest.plan_bytes(rig.guest.boots() - 1));
    assert!(seen.contains_key("PUDDLE_AGENT_EDITOR_DIR"), "{seen:?}");
    assert_eq!(seen.get("PUDDLE_AGENT_EDITOR_USER"), None, "{seen:?}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_guest_gets_its_ssh_agent_socket_from_the_first_start_with_a_host_switched_on() {
    const SSH_CONFIG: &str = "/etc/ssh/ssh_config.d/00-puddle.conf";
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let n = rig.guest.boots() - 1;
    let seen = agent_settings(&rig.guest.plan_bytes(n));
    assert_eq!(seen.get("PUDDLE_AGENT_SSH_AGENT"), None, "{seen:?}");
    let files = rig.guest.plan_files(n);
    let ssh = String::from_utf8_lossy(&files[SSH_CONFIG]).into_owned();
    assert!(!ssh.contains("IdentityAgent"), "{ssh}");
    assert!(
        files
            .values()
            .all(|bytes| !String::from_utf8_lossy(bytes).contains("SSH_AUTH_SOCK")),
        "no socket, no variable"
    );

    // The switch applies to the proxy at once; the guest's socket waits for the next start.
    let put = api
        .put(
            "/api/workspaces/acme/ssh-agent/build.example.com",
            &json!({"keys": null}).to_string(),
        )
        .await;
    assert_eq!(put.status, 200, "{}", put.body);
    for step in ["stop", "start"] {
        let reply = api.post(&format!("/api/workspaces/acme/{step}"), "").await;
        assert_eq!(reply.status, 202, "{}", reply.body);
        events.until(ended("acme"), Duration::from_secs(20)).await;
    }
    let n = rig.guest.boots() - 1;
    let seen = agent_settings(&rig.guest.plan_bytes(n));
    assert_eq!(
        seen.get("PUDDLE_AGENT_SSH_AGENT").map(String::as_str),
        Some("/run/puddle/ssh-agent.sock")
    );
    assert_eq!(
        seen.get("PUDDLE_AGENT_SSH_AGENT_USER").map(String::as_str),
        Some("agent")
    );
    let files = rig.guest.plan_files(n);
    let ssh = String::from_utf8_lossy(&files[SSH_CONFIG]).into_owned();
    assert!(
        ssh.contains("    IdentityAgent \"/run/puddle/ssh-agent.sock\"\n"),
        "{ssh}"
    );
    assert!(
        files
            .values()
            .any(|bytes| String::from_utf8_lossy(bytes).contains("SSH_AUTH_SOCK")),
        "the variable reaches the guest"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workspaces_reconnection_grace_reaches_the_editor_server_at_the_next_start() {
    let rig = Rig::named_ids();
    let mut config = rig.config();
    config.guest.code_server = Some(fake_code_server(&rig));
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    let reply = api
        .put(
            "/api/settings/workspaces/acme",
            &json!({"overrides": {"reconnection_grace": 90}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let seen = agent_settings(&rig.guest.plan_bytes(rig.guest.boots() - 1));
    assert_eq!(
        seen.get("PUDDLE_AGENT_EDITOR_GRACE").map(String::as_str),
        Some("90")
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_editor_defaults_reach_the_guest_as_one_file_and_leave_with_the_last_one() {
    const FILE: &str = "/etc/puddle/editor-defaults.json";
    let rig = Rig::named_ids();
    let mut config = rig.config();
    config.guest.code_server = Some(fake_code_server(&rig));
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let last = || rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!last().contains_key(FILE), "nothing set, no file");

    stop_and_wait(&api, &mut events, "acme").await;
    let global = api
        .put(
            "/api/settings",
            &json!({"workspace_defaults": {"editor": {"colour_theme": "Default Dark Modern", "font_size": 15}}})
                .to_string(),
        )
        .await;
    assert_eq!(global.status, 200, "{}", global.body);
    let own = api
        .put(
            "/api/settings/workspaces/acme",
            &json!({"overrides": {"editor": {"font_size": 18, "extensions": ["Rust-Lang.Rust-Analyzer"]}}})
                .to_string(),
        )
        .await;
    assert_eq!(own.status, 200, "{}", own.body);
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let file = last().remove(FILE).expect("the defaults file is planned");
    let planned: serde_json::Value = serde_json::from_slice(&file).unwrap();
    assert_eq!(
        planned,
        json!({"colour_theme": "Default Dark Modern", "font_size": 18, "extensions": ["rust-lang.rust-analyzer"]})
    );

    stop_and_wait(&api, &mut events, "acme").await;
    for body in
        [json!({"workspace_defaults": {"editor": {"colour_theme": null, "font_size": null}}})]
    {
        let reply = api.put("/api/settings", &body.to_string()).await;
        assert_eq!(reply.status, 200, "{}", reply.body);
    }
    let reply = api
        .put(
            "/api/settings/workspaces/acme",
            &json!({"overrides": {"editor": {"font_size": null, "extensions": null}}}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(!last().contains_key(FILE), "unset again, no file");
    host.shutdown().await;
}

/// What the first boot of `acme` told the agent about its editor server.
async fn editor_env_of_a_new_workspace(
    rig: &Rig,
    config: HostConfig,
) -> std::collections::BTreeMap<String, String> {
    let host = rig.start_with(config).await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let seen = agent_settings(&rig.guest.plan_bytes(rig.guest.boots() - 1));
    host.shutdown().await;
    seen
}

#[tokio::test(flavor = "multi_thread")]
async fn microsofts_server_runs_after_consent_with_its_own_folder_and_the_telemetry_choice() {
    for (telemetry, auto_update) in [(false, true), (true, false)] {
        let rig = Rig::named_ids();
        let mut doc = microsoft_chosen(puddle_settings::MICROSOFT_TERMS_VERSION, telemetry);
        doc["vscode_server"]["auto_update"] = json!(auto_update);
        write_global_settings(&rig, &doc);
        // No code-server bundled: Microsoft's is downloaded by the guest, not shipped.
        let seen = editor_env_of_a_new_workspace(&rig, rig.config()).await;
        let get = |name: &str| seen.get(name).map(String::as_str);
        assert_eq!(get("PUDDLE_AGENT_EDITOR_KIND"), Some("microsoft"));
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_TELEMETRY"),
            Some(if telemetry { "1" } else { "0" })
        );
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_AUTO_UPDATE"),
            Some(if auto_update { "1" } else { "0" })
        );
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_DIR"),
            Some("/workspace/.puddle/vscode-server/servers")
        );
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_DATA"),
            Some("/workspace/.puddle/vscode-server")
        );
        assert_eq!(get("PUDDLE_AGENT_EDITOR_FOLDER"), Some("/workspace"));
        assert_eq!(get("PUDDLE_AGENT_EDITOR_GRACE"), Some("300"));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn microsofts_server_never_starts_on_the_choice_alone() {
    // The recorded consent is what counts: none, a decline and an agreement to other terms each
    // leave the guest with code-server, and no word about Microsoft's server reaches it.
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    for (what, consents) in [
        ("no consent", json!({})),
        (
            "a decline",
            json!({"vscode_server": {"state": "declined", "at": 1, "terms_version": covering}}),
        ),
        (
            "other terms",
            json!({"vscode_server": {"state": "granted", "at": 1, "terms_version": "https://example.test/old"}}),
        ),
    ] {
        let rig = Rig::named_ids();
        write_global_settings(
            &rig,
            &json!({
                "schema_version": 2,
                "vscode_server": {"server": "microsoft", "telemetry": true},
                "consents": consents,
            }),
        );
        let mut config = rig.config();
        config.guest.code_server = Some(fake_code_server(&rig));
        let seen = editor_env_of_a_new_workspace(&rig, config).await;
        let get = |name: &str| seen.get(name).map(String::as_str);
        assert_eq!(
            get("PUDDLE_AGENT_EDITOR_DIR"),
            Some("/puddle/code-server"),
            "{what}"
        );
        assert_eq!(get("PUDDLE_AGENT_EDITOR_KIND"), None, "{what}");
        assert_eq!(get("PUDDLE_AGENT_EDITOR_TELEMETRY"), None, "{what}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_code_server_and_without_microsofts_choice_runs_no_editor_server() {
    let rig = Rig::named_ids();
    // Consent recorded, choice not made: code-server, which this host does not bundle.
    write_global_settings(
        &rig,
        &json!({
            "schema_version": 2,
            "consents": {"vscode_server": {"state": "granted", "at": 1, "terms_version": puddle_settings::MICROSOFT_TERMS_VERSION}},
        }),
    );
    let seen = editor_env_of_a_new_workspace(&rig, rig.config()).await;
    assert!(seen.is_empty(), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_without_code_server_starts_guests_without_an_editor_server() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let plan = String::from_utf8(rig.guest.plan_bytes(rig.guest.boots() - 1)).unwrap();
    assert!(!plan.contains("puddle_agent_env"), "{plan}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_commit_authors_in_the_running_guest_follow_the_identities() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    // No identity yet: no author file, and the workspace's git settings name nobody.
    let files = rig.guest.plan_files(booted - 1);
    assert!(!pem_of(&files, "/etc/puddle/gitconfig").contains("[user]"));

    // Work names acme on github.com; Personal covers the rest of it and is first in the list.
    let personal = make_identity(&api, "Personal", "github.com", &[]).await;
    let work = make_identity(&api, "Work", "github.com", &["Acme"]).await;
    attach(&api, "acme", personal).await;
    rig.guest.boots_reach(booted + 1).await;
    attach(&api, "acme", work).await;
    rig.guest.boots_reach(booted + 2).await;
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let config = pem_of(&files, "/etc/puddle/gitconfig");
    // The fallback is the first identity's; the rest-of-host rule comes before the named owner's,
    // so git, which lets the last match win, gives acme's remotes to Work.
    assert!(config.contains("[user]\n\tname = \"Personal\""), "{config}");
    let personal_file = author_file(&files, "Personal");
    let work_file = author_file(&files, "Work");
    let at = |path: &str| config.find(&format!("path = {}", path.rsplit('/').next().unwrap()));
    let (personal_at, work_at) = (at(&personal_file).unwrap(), at(&work_file).unwrap());
    assert!(personal_at < work_at, "{config}");
    assert!(config.contains("[aA][cC][mM][eE]"), "{config}");
    // The guest's trust is what it booted with: the same CA in every run.
    let ca = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    assert_eq!(pem_of(&files, "/etc/puddle/extra-cas.pem"), ca.pem());

    // A change that moves no author does not touch the guest again.
    let reply = api
        .request(
            "PUT",
            "/api/workspaces/acme/git/switches",
            Some(&json!({"only_push_listed": false}).to_string()),
            true,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    // Absence window: a change that moves no author must not boot the guest again.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(rig.guest.boots(), booted + 2);

    // Taking both off removes the authors again. One at a time, each seen in the guest: two
    // changes made before the host looks at the first reach the guest as one run, and how many
    // runs that is depends on timing, not on the contract.
    for (n, id) in [personal, work].into_iter().enumerate() {
        api.delete(&format!("/api/workspaces/acme/identities/{id}"), None)
            .await;
        rig.guest.boots_reach(booted + 3 + n).await;
    }
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!pem_of(&files, "/etc/puddle/gitconfig").contains("[user]"));
    assert!(files.keys().all(|path| !path.contains("git-author-")));
    host.shutdown().await;
}

/// The guest is Linux, so no byte Puddle puts into it may be a carriage return, whatever line
/// ends the host's own text uses (the certificate text of a Windows build ends its lines with
/// CRLF unless it is made with LF on purpose).
#[tokio::test(flavor = "multi_thread")]
async fn nothing_the_boot_plan_puts_into_the_guest_has_a_carriage_return() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let booted = rig.guest.boots();
    // Two identities, one naming an owner: the CA files, the environment file, the git settings
    // and both kinds of author file all end up in the plan.
    let personal = make_identity(&api, "Personal", "github.com", &[]).await;
    let work = make_identity(&api, "Work", "github.com", &["Acme"]).await;
    attach(&api, "acme", personal).await;
    rig.guest.boots_reach(booted + 1).await;
    attach(&api, "acme", work).await;
    rig.guest.boots_reach(booted + 2).await;

    for n in 0..rig.guest.boots() {
        let plan = rig.guest.plan_bytes(n);
        assert!(!plan.contains(&b'\r'), "plan {n} has a carriage return");
        for (path, bytes) in rig.guest.plan_files(n) {
            assert!(!bytes.contains(&b'\r'), "plan {n}: {path}");
        }
    }
    // The scan saw what it is about: the last plan has every file kind named above.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    for path in [
        "/etc/puddle/extra-cas.pem",
        "/etc/puddle/puddle-cas.pem",
        "/usr/local/share/ca-certificates/puddle/puddle-ca-1.crt",
        "/etc/profile.d/01-puddle-env.sh",
        "/etc/puddle/gitconfig",
    ] {
        assert!(
            files.contains_key(path),
            "{path} is not in {:?}",
            files.keys()
        );
    }
    author_file(&files, "Personal");
    author_file(&files, "Work");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_workspace_has_no_ca_and_each_start_makes_a_new_one() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let first = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();

    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(host.workspaces().termination(&name("acme")).is_none());
    // A change to a workspace that does not run reaches nothing now: its next start reads it.
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    attach(&api, "acme", identity).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(host.workspaces().termination(&name("acme")).is_none());

    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let second = host.workspaces().termination(&name("acme")).unwrap();
    assert_ne!(second.ca().certificate(), &first);
    assert!(decrypts(&host, "acme", "github.com"));
    // The second boot got the second CA, and not the first.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert_eq!(
        pem_of(&files, "/etc/puddle/extra-cas.pem"),
        second.ca().certificate().pem()
    );
    assert!(!pem_of(&files, "/etc/puddle/extra-cas.pem").contains(first.pem()));
    // The author came from the identity at the start, with no rewrite after it.
    assert!(pem_of(&files, "/etc/puddle/gitconfig").contains("name = \"Ada\""));
    host.shutdown().await;
}

/// The skipped file is one ed25519 key, the one whose public half is `login_ed25519.pub`: a CA
/// key written under that name would not hide behind the exclusion.
fn assert_is_the_login_key(file: &std::path::Path) {
    use base64::Engine as _;
    let engine = base64::engine::general_purpose::STANDARD;
    let public = std::fs::read_to_string(file.with_extension("pub")).unwrap();
    let public_blob = engine.decode(public.split(' ').nth(1).unwrap()).unwrap();
    let text = std::fs::read_to_string(file).unwrap();
    let lines: Vec<_> = text.lines().collect();
    // The armor lines are spelled in parts: the repository's secret scan looks for the whole.
    let kind = ["OPENSSH", "PRIVATE", "KEY"].join(" ");
    assert_eq!(
        lines.first().copied(),
        Some(format!("-----BEGIN {kind}-----").as_str())
    );
    assert_eq!(
        lines.last().copied(),
        Some(format!("-----END {kind}-----").as_str())
    );
    let body = engine.decode(lines[1..lines.len() - 1].concat()).unwrap();
    assert!(body.starts_with(b"openssh-key-v1\0"));
    assert_eq!(
        body.windows(public_blob.len())
            .filter(|w| *w == public_blob)
            .count(),
        2,
        "the public half is in the file twice (header and key section): one key"
    );
    assert_eq!(
        public_blob.len(),
        4 + b"ssh-ed25519".len() + 4 + 32,
        "an ed25519 public half"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_file_in_the_data_folder_holds_a_key_or_the_ca_whether_the_workspace_runs_or_not() {
    fn files_under(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files_under(&path, out);
            } else {
                out.push(path);
            }
        }
    }
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    attach(&api, "acme", identity).await;
    eventually("decrypted", APPLY, || decrypts(&host, "acme", "github.com")).await;
    let ca = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    let body = ca.pem().lines().nth(1).unwrap().to_owned();

    let check = |when: &str| {
        let mut files = Vec::new();
        files_under(&rig.dir.path().join("data"), &mut files);
        files_under(&rig.dir.path().join("runtime"), &mut files);
        assert!(!files.is_empty());
        for file in files {
            // The one key file Puddle keeps is direct SSH's login key: `ssh` reads its identity from
            // a file, so it is owner-only on disk by design, and it opens nothing but the runtime's
            // SSH server. It is made at every start, whether or not the switch is on.
            if file == rig.dir.path().join("data/ssh/login_ed25519") {
                assert_is_the_login_key(&file);
                continue;
            }
            // A file the database or a save is replacing may be gone by now.
            let Ok(bytes) = std::fs::read(&file) else {
                continue;
            };
            assert!(
                !bytes.windows(KEY_MARKER.len()).any(|w| w == KEY_MARKER),
                "{when}: {} holds a key",
                file.display()
            );
            assert!(
                !bytes.windows(body.len()).any(|w| w == body.as_bytes()),
                "{when}: {} holds the CA",
                file.display()
            );
            assert!(
                !bytes
                    .windows(ca.der().len())
                    .any(|w| w == ca.der().as_ref()),
                "{when}: {} holds the CA",
                file.display()
            );
        }
    };
    check("running");
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    check("stopped");
    // A workspace still running at shutdown loses its CA with the host's routes.
    api.post(&format!("/api/workspaces/{}/start", "acme"), "")
        .await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert!(host.workspaces().termination(&name("acme")).is_some());
    host.shutdown().await;
    assert!(host.workspaces().termination(&name("acme")).is_none());
    check("after shutdown");
}

#[derive(Debug)]
struct MarkedInjector;

impl puddle_proxy::Injector for MarkedInjector {
    fn decide<'a>(
        &'a self,
        _: &'a puddle_proxy::InjectContext<'a>,
        _: &'a puddle_proxy::RequestView<'a>,
    ) -> futures_util::future::BoxFuture<'a, puddle_proxy::InjectDecision> {
        Box::pin(async { puddle_proxy::InjectDecision::PassThrough })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn each_start_gets_an_injector_built_for_its_workspace_from_the_hosts_store() {
    let rig = Rig::named_ids();
    let built = Arc::new(Mutex::new(Vec::new()));
    let prepared = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap();
    let mut options = named_id_options();
    let record = built.clone();
    options.injector = Some(Arc::new(
        move |inputs: &puddle_host::InjectorInputs, workspace: &WorkspaceId| {
            record
                .lock()
                .unwrap()
                .push((workspace.clone(), inputs.store.clone()));
            Arc::new(MarkedInjector)
        },
    ));
    let host = Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap();
    // Nothing is made before a workspace starts.
    assert!(built.lock().unwrap().is_empty());
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    for workspace in ["acme", "beta"] {
        let termination = host.workspaces().termination(&name(workspace)).unwrap();
        assert!(
            format!("{termination:?}").contains("MarkedInjector"),
            "{termination:?}"
        );
    }
    let made = built.lock().unwrap().clone();
    let workspaces: Vec<&WorkspaceId> = made.iter().map(|(w, _)| w).collect();
    assert_eq!(workspaces, [&name("acme"), &name("beta")]);
    assert!(
        made.iter()
            .all(|(_, store)| Arc::ptr_eq(store, host.store()))
    );
    // A start in place makes a new one.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(built.lock().unwrap().len(), 3);
    host.shutdown().await;
}
