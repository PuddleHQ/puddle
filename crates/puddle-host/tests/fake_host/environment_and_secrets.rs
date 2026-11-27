// SPDX-License-Identifier: AGPL-3.0-or-later
//! Variables, secrets and their stand-ins.

use super::*;

#[tokio::test]
async fn missed_events_are_made_up_for_by_looking_at_every_running_workspace_again() {
    // One thread: the host's tasks cannot run while this test emits, so the follower falls
    // behind the event buffer.
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let identity = make_identity(&api, "Ada", "github.com", &[]).await;
    let before = rig.guest.boots();

    let attached =
        host.store()
            .attach_identity(&name("acme"), puddle_store::IdentityId(identity), None);
    attached.unwrap();
    for _ in 0..4096 {
        host.events().emit(Event::RulesChanged {});
    }
    eventually("the set follows", APPLY, || {
        decrypts(&host, "acme", "github.com")
    })
    .await;
    rig.guest.boots_reach(before + 1).await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_starts_with_its_variables_and_a_stand_in_for_each_secret_and_the_real_value_never_reaches_the_guest()
 {
    let logs = support::capture_logs();
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    put_env(
        &api,
        "/api/env/NPM_TOKEN",
        secret_body(Some(REAL), &["registry.example.org"]),
    )
    .await;
    create(&api, &mut events, "acme").await;
    put_env(
        &api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(Some(REAL), &["api.example.org", "*.example.net"]),
    )
    .await;
    // The second secret was added to a running workspace: its stand-in reaches the guest at the
    // next start, so start again to see it in the guest.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;

    let env = rig.guest.boot_envs.lock().unwrap().last().cloned().unwrap();
    assert_eq!(env.get("EDITOR"), Some("vim"));
    let npm = env.get("NPM_TOKEN").unwrap();
    let key = env.get("API_KEY").unwrap();
    assert!(npm.starts_with("puddle-secret-NPM_TOKEN-"), "{npm}");
    assert!(key.starts_with("puddle-secret-API_KEY-"), "{key}");
    assert_eq!(npm, stand_in_of(&host, "acme", "NPM_TOKEN"));
    // Puddle's own variables are still there beside them.
    assert_eq!(env.get("HTTPS_PROXY"), Some("http://127.0.0.1:3128"));
    // Login shells see the same through the env file.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let file = pem_of(&files, "/etc/profile.d/01-puddle-env.sh");
    assert!(
        file.contains(&format!("export NPM_TOKEN='{npm}'")),
        "{file}"
    );
    assert!(file.contains("export EDITOR='vim'"), "{file}");

    // The proxy holds both, for their own hosts, and decrypts those hosts.
    let termination = host.workspaces().termination(&name("acme")).unwrap();
    assert_eq!(
        termination.stand_ins().ids(),
        ["stand-in:secret:API_KEY", "stand-in:secret:NPM_TOKEN"]
    );
    for site in ["registry.example.org", "api.example.org", "x.example.net"] {
        assert!(decrypts(&host, "acme", site), "{site}");
    }
    assert!(!decrypts(&host, "acme", "example.net"));

    // The real value is in the credential store and nowhere the guest or a log has been.
    assert_eq!(rig.secrets.ids().len(), 2);
    assert!(!guest_has(&rig, REAL), "the real value reached the guest");
    assert!(
        !guest_has(&rig, "CANARY"),
        "a piece of the real value reached the guest"
    );
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(!logged.contains("CANARY"), "the real value was logged");
    host.shutdown().await;
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(
        !logged.contains("CANARY"),
        "the real value was logged at shutdown"
    );
}

/// A running host with two running workspaces, `acme` and `beta`.
struct TwoWorkspaces {
    rig: Rig,
    host: Host<FakeRuntime>,
    api: Api,
}

async fn acme_and_beta() -> TwoWorkspaces {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    create(&api, &mut events, "beta").await;
    TwoWorkspaces { rig, host, api }
}

/// A secret for a host the workspace has never used and no identity names.
async fn put_acme_api_key(w: &TwoWorkspaces) {
    put_env(
        &w.api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(Some(REAL), &["api.example.org"]),
    )
    .await;
    eventually("the secret's host is decrypted", APPLY, || {
        decrypts(&w.host, "acme", "api.example.org")
    })
    .await;
}

async fn put_global_shared(w: &TwoWorkspaces) {
    put_env(
        &w.api,
        "/api/env/SHARED",
        secret_body(Some(REAL), &["shared.example.org"]),
    )
    .await;
    for workspace in ["acme", "beta"] {
        eventually("the global secret's host is decrypted", APPLY, || {
            decrypts(&w.host, workspace, "shared.example.org")
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_secret_added_to_a_running_workspace_is_decrypted_from_the_next_connection_with_no_restart()
 {
    let w = acme_and_beta().await;
    let (rig, host) = (&w.rig, &w.host);
    let ca_before = host
        .workspaces()
        .termination(&name("acme"))
        .unwrap()
        .ca()
        .certificate()
        .clone();
    let registry_before = host.workspaces().termination(&name("acme")).unwrap();
    let beta_commands = rig
        .guest
        .commands()
        .iter()
        .filter(|c| c.starts_with("beta "))
        .count();
    let (creates, starts, stops) = (
        calls_of(rig, "Create"),
        calls_of(rig, "Start"),
        calls_of(rig, "Stop"),
    );
    assert!(!decrypts(host, "acme", "api.example.org"));

    put_acme_api_key(&w).await;
    let after = host.workspaces().termination(&name("acme")).unwrap();
    assert_eq!(after.stand_ins().ids(), ["stand-in:secret:API_KEY"]);
    // The same CA and the same registry: the guest's trust is unchanged and open connections see
    // the new stand-in too.
    assert_eq!(after.ca().certificate(), &ca_before);
    assert!(Arc::ptr_eq(registry_before.stand_ins(), after.stand_ins()));
    // Only that workspace, and nothing was started or stopped. (The login-shell file is rewritten
    // for a change to the variables, which runs the boot plan again in that guest alone; the next
    // test is about that.)
    assert!(!decrypts(host, "beta", "api.example.org"));
    assert_eq!(
        rig.guest
            .commands()
            .iter()
            .filter(|c| c.starts_with("beta "))
            .count(),
        beta_commands,
        "beta's guest was not touched"
    );
    assert_eq!(
        (
            calls_of(rig, "Create"),
            calls_of(rig, "Start"),
            calls_of(rig, "Stop")
        ),
        (creates, starts, stops)
    );
    w.host.shutdown().await;
}

const SECOND: &str = "ghp_CANARY-second-token-swapped-in-while-it-runs";

/// A new secret value saved while a build runs. The guest keeps the same stand-in,
/// nothing restarts, the registry is the same one the open connections use, and neither value
/// reaches the guest or a log. That the next request carries the new value is
/// `a_new_real_value_for_a_stand_in_applies_from_the_next_request_on_an_open_connection` in
/// `puddle-proxy`.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_secret_value_saved_while_it_runs_keeps_the_stand_in_and_restarts_nothing() {
    let logs = support::capture_logs();
    let w = acme_and_beta().await;
    let (rig, host) = (&w.rig, &w.host);
    put_acme_api_key(&w).await;
    let stand_in = stand_in_of(host, "acme", "API_KEY");
    let registry = host.workspaces().termination(&name("acme")).unwrap();
    let boots = rig.guest.boots();
    let (creates, starts, stops) = (
        calls_of(rig, "Create"),
        calls_of(rig, "Start"),
        calls_of(rig, "Stop"),
    );

    // Only the value changes; the hosts are sent as they were.
    put_env(
        &w.api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(Some(SECOND), &["api.example.org"]),
    )
    .await;
    rig.guest.boots_reach(boots + 1).await;

    assert_eq!(stand_in_of(host, "acme", "API_KEY"), stand_in);
    let after = host.workspaces().termination(&name("acme")).unwrap();
    assert!(Arc::ptr_eq(registry.stand_ins(), after.stand_ins()));
    assert_eq!(after.stand_ins().ids(), ["stand-in:secret:API_KEY"]);
    assert!(decrypts(host, "acme", "api.example.org"), "the host stays");
    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(
        file.contains(&format!("export API_KEY='{stand_in}'")),
        "{file}"
    );
    assert_eq!(rig.secrets.ids().len(), 1, "one value, replaced");
    assert_eq!(
        (
            calls_of(rig, "Create"),
            calls_of(rig, "Start"),
            calls_of(rig, "Stop")
        ),
        (creates, starts, stops)
    );
    for value in [REAL, SECOND, "CANARY"] {
        assert!(!guest_has(rig, value), "a value reached the guest");
    }
    w.host.shutdown().await;
    let logged = String::from_utf8_lossy(&logs.lock().unwrap()).into_owned();
    assert!(!logged.contains("CANARY"), "a value was logged");
}

/// A plain variable added while a process runs reaches the login-shell file (a new
/// terminal reads it) and not the sandbox's own environment, which a running process and every
/// exec keep until the next start.
#[tokio::test(flavor = "multi_thread")]
async fn a_plain_variable_added_while_it_runs_reaches_new_terminals_and_not_the_sandbox_environment()
 {
    let w = acme_and_beta().await;
    let rig = &w.rig;
    let boots = rig.guest.boots();
    let (creates, starts, stops, set_envs) = (
        calls_of(rig, "Create"),
        calls_of(rig, "Start"),
        calls_of(rig, "Stop"),
        calls_of(rig, "SetEnv"),
    );
    assert_eq!(
        rig.guest
            .boot_envs
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .get("QA_MODE"),
        None
    );

    put_env(
        &w.api,
        "/api/workspaces/acme/env/QA_MODE",
        json!({"kind": "plain", "value": "two"}),
    )
    .await;
    rig.guest.boots_reach(boots + 1).await;

    let file = pem_of(
        &rig.guest.plan_files(rig.guest.boots() - 1),
        "/etc/profile.d/01-puddle-env.sh",
    );
    assert!(file.contains("export QA_MODE='two'"), "{file}");
    // What a process already running (and the next exec) is started from has not changed.
    assert!(
        rig.guest
            .boot_envs
            .lock()
            .unwrap()
            .iter()
            .all(|env| env.get("QA_MODE").is_none()),
        "the sandbox's environment was changed under a running process"
    );
    assert_eq!(calls_of(rig, "SetEnv"), set_envs, "no environment write");
    assert_eq!(
        (
            calls_of(rig, "Create"),
            calls_of(rig, "Start"),
            calls_of(rig, "Stop")
        ),
        (creates, starts, stops)
    );
    w.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_global_secret_reaches_every_running_workspace_with_its_own_stand_in() {
    let w = acme_and_beta().await;
    put_global_shared(&w).await;
    // Each holds its own stand-in for it.
    assert_ne!(
        stand_in_of(&w.host, "acme", "SHARED"),
        stand_in_of(&w.host, "beta", "SHARED")
    );
    w.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn changed_hosts_move_the_decrypted_host_and_removing_the_secrets_takes_them_out_again() {
    let w = acme_and_beta().await;
    let (rig, host, api) = (&w.rig, &w.host, &w.api);
    put_acme_api_key(&w).await;
    put_global_shared(&w).await;

    // Changed hosts move the decrypted host; removing the secret takes it out again.
    put_env(
        api,
        "/api/workspaces/acme/env/API_KEY",
        secret_body(None, &["other.example.org"]),
    )
    .await;
    eventually("the hosts moved", APPLY, || {
        decrypts(host, "acme", "other.example.org") && !decrypts(host, "acme", "api.example.org")
    })
    .await;
    assert_eq!(
        api.delete("/api/workspaces/acme/env/API_KEY", None)
            .await
            .status,
        204
    );
    assert_eq!(api.delete("/api/env/SHARED", None).await.status, 204);
    eventually("removed secrets are not decrypted", APPLY, || {
        !decrypts(host, "acme", "other.example.org")
            && !decrypts(host, "acme", "shared.example.org")
            && !decrypts(host, "beta", "shared.example.org")
    })
    .await;
    assert!(
        host.workspaces()
            .termination(&name("acme"))
            .unwrap()
            .stand_ins()
            .ids()
            .is_empty()
    );
    assert!(
        rig.secrets.ids().is_empty(),
        "no value left in the credential store"
    );
    w.host.shutdown().await;
}

/// A running host with the global variable `EDITOR=vim` and a running workspace `acme` made under it.
struct EditorRig {
    rig: Rig,
    host: Host<FakeRuntime>,
    api: Api,
    events: support::Events,
    volumes: Vec<String>,
    creates: usize,
}

async fn editor_rig() -> EditorRig {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/EDITOR",
        json!({"kind": "plain", "value": "vim"}),
    )
    .await;
    create(&api, &mut events, "acme").await;
    let volumes = volume_names(&rig).await;
    assert_eq!(
        rig.guest.boot_envs.lock().unwrap()[0].get("EDITOR"),
        Some("vim")
    );
    let creates = calls_of(&rig, "Create");
    EditorRig {
        rig,
        host,
        api,
        events,
        volumes,
        creates,
    }
}

async fn stop_acme(e: &mut EditorRig) {
    e.api.post("/api/workspaces/acme/stop", "").await;
    e.events.until(ended("acme"), Duration::from_secs(20)).await;
}

async fn start_acme(e: &mut EditorRig) -> serde_json::Value {
    e.api.post("/api/workspaces/acme/start", "").await;
    e.events.until(ended("acme"), Duration::from_secs(20)).await
}

async fn set_editor(e: &EditorRig, value: &str) {
    put_env(
        &e.api,
        "/api/env/EDITOR",
        json!({"kind": "plain", "value": value}),
    )
    .await;
}

fn last_boot_env(e: &EditorRig, var: &str) -> Option<String> {
    e.rig
        .guest
        .boot_envs
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .get(var)
        .map(str::to_owned)
}

fn said_made_again(e: &EditorRig, also: &str) -> bool {
    e.events.seen.iter().any(|ev| {
        ev["type"] == "workspace_progress"
            && ev["detail"]
                .as_str()
                .is_some_and(|d| d.contains("made again") && d.contains(also))
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unchanged_environment_starts_the_stopped_sandbox_again_and_does_not_make_it_again() {
    let mut e = editor_rig().await;
    stop_acme(&mut e).await;
    start_acme(&mut e).await;
    assert_eq!(calls_of(&e.rig, "Create"), e.creates);
    assert_eq!(calls_of(&e.rig, "Remove"), 0);
    assert!(
        e.host
            .problems()
            .list()
            .iter()
            .all(|p| p.key != "rebuilt:acme"),
        "a kept sandbox is no rebuild"
    );
    e.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_variable_is_written_to_the_stopped_sandbox_and_keeps_the_root_disk() {
    let mut e = editor_rig().await;
    stop_acme(&mut e).await;
    set_editor(&e, "nano").await;
    let set_envs = calls_of(&e.rig, "SetEnv");
    let end = start_acme(&mut e).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(calls_of(&e.rig, "Create"), e.creates);
    assert_eq!(calls_of(&e.rig, "Remove"), 0);
    assert_eq!(calls_of(&e.rig, "SetEnv"), set_envs + 1);
    assert!(
        !said_made_again(&e, ""),
        "a kept root disk needs no warning"
    );
    assert_eq!(last_boot_env(&e, "EDITOR").as_deref(), Some("nano"));
    // Puddle's own variables are written with them, not dropped.
    assert_eq!(
        last_boot_env(&e, "HTTPS_PROXY").as_deref(),
        Some("http://127.0.0.1:3128")
    );
    assert_eq!(volume_names(&e.rig).await, e.volumes);

    // The same variables at the next start write nothing more.
    stop_acme(&mut e).await;
    start_acme(&mut e).await;
    assert_eq!(calls_of(&e.rig, "Create"), e.creates);
    assert_eq!(calls_of(&e.rig, "SetEnv"), set_envs + 1);
    e.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_environment_write_falls_back_to_a_new_sandbox_on_the_same_volume_and_says_so() {
    let mut e = editor_rig().await;
    stop_acme(&mut e).await;
    set_editor(&e, "emacs").await;
    e.rig.runtime.inject(
        Op::SetEnv,
        Fault::once(disk_error("set the environment of")),
    );
    let end = start_acme(&mut e).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(calls_of(&e.rig, "Create"), e.creates + 1);
    assert_eq!(calls_of(&e.rig, "Remove"), 1);
    assert!(
        said_made_again(&e, "volume"),
        "no notice that the sandbox was made again"
    );
    assert_eq!(last_boot_env(&e, "EDITOR").as_deref(), Some("emacs"));
    assert_eq!(volume_names(&e.rig).await, e.volumes);
    let lasting = e
        .host
        .problems()
        .list()
        .into_iter()
        .find(|p| p.key == "rebuilt:acme")
        .expect("the rebuild stays on the workspace as a problem after the start ended");
    assert!(
        lasting.detail.contains("variables") && lasting.detail.contains("installed packages"),
        "{lasting:?}"
    );

    // A start that keeps the sandbox does not take it back: the root disk stays lost.
    stop_acme(&mut e).await;
    start_acme(&mut e).await;
    assert!(
        e.host
            .problems()
            .list()
            .iter()
            .any(|p| p.key == "rebuilt:acme")
    );
    e.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stand_in_is_the_same_at_every_start_so_files_the_workspace_wrote_with_it_keep_working() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some(REAL), &["a.example.org"]),
    )
    .await;
    create(&api, &mut events, "acme").await;
    let first = rig
        .guest
        .boot_envs
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .get("TOKEN")
        .unwrap()
        .to_owned();
    // A new value and new hosts keep it.
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some("rotated"), &["b.example.org"]),
    )
    .await;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let second = rig
        .guest
        .boot_envs
        .lock()
        .unwrap()
        .last()
        .unwrap()
        .get("TOKEN")
        .unwrap()
        .to_owned();
    assert_eq!(first, second);
    assert_eq!(
        calls_of(&rig, "Create"),
        1,
        "same environment, so the same sandbox restarted"
    );
    // The stand-in survives a restart of Puddle too: it is in the database.
    host.shutdown().await;
    // The next start is a new process: the first one's hold on the data folder ends.
    drop(host);
    let host = rig.start().await;
    assert_eq!(stand_in_of(&host, "acme", "TOKEN"), first);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_whose_secret_cannot_be_read_fails_naming_it_and_works_once_it_is_set_again() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some(REAL), &["a.example.org"]),
    )
    .await;
    // The value is gone from the credential store (the data folder came from another computer).
    for id in rig.secrets.ids() {
        rig.secrets
            .delete(&puddle_secrets::StoredId::new(&id).unwrap())
            .unwrap();
    }
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "failed", "{end}");
    let detail = end["detail"].as_str().unwrap();
    assert!(
        detail.contains("TOKEN") && detail.contains("Environment tab"),
        "{detail}"
    );
    assert!(!detail.contains(REAL));
    assert!(
        host.workspaces().termination(&name("acme")).is_none(),
        "nothing is registered"
    );

    // Setting the secret again fixes it.
    put_env(
        &api,
        "/api/env/TOKEN",
        secret_body(Some(REAL), &["a.example.org"]),
    )
    .await;
    create(&api, &mut events, "again").await;
    assert!(host.workspaces().termination(&name("again")).is_some());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_variables_puddle_adds_to_keep_the_users_value_and_the_ones_it_owns_stay_puddles() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/JAVA_TOOL_OPTIONS",
        json!({"kind": "plain", "value": "-Xmx1g"}),
    )
    .await;
    put_env(
        &api,
        "/api/env/DOCKER_CONFIG",
        json!({"kind": "plain", "value": "/data/docker"}),
    )
    .await;
    // A name Puddle owns cannot be set through the API; a hand-edited database row is ignored in
    // favour of Puddle's own value.
    host.store()
        .set_env(
            puddle_store::UserId::OWNER,
            &puddle_store::EnvScope::Global,
            &puddle_store::EnvName::existing("HTTPS_PROXY").unwrap(),
            puddle_store::EnvDraft::plain("http://elsewhere:1").unwrap(),
        )
        .unwrap();
    create(&api, &mut events, "acme").await;

    let env = rig.guest.boot_envs.lock().unwrap().last().cloned().unwrap();
    let java = env.get("JAVA_TOOL_OPTIONS").unwrap();
    assert!(java.starts_with("-Xmx1g "), "{java}");
    assert!(java.contains("-Dhttp.proxyHost=127.0.0.1"), "{java}");
    assert_eq!(env.get("HTTPS_PROXY"), Some("http://127.0.0.1:3128"));
    // The Docker CLI's proxy settings follow `DOCKER_CONFIG` to where the user put it.
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!files.contains_key("/root/.docker/config.json"));
    assert!(
        guest_has(&rig, "/data/docker/config.json"),
        "the Docker CLI's proxy settings did not follow DOCKER_CONFIG"
    );
    assert!(!guest_has(&rig, "/root/.docker/config.json"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_removes_its_variables_rules_and_the_values_of_its_own_secrets() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    put_env(
        &api,
        "/api/env/GLOBAL_TOKEN",
        secret_body(Some(REAL), &["g.example.org"]),
    )
    .await;
    create(&api, &mut events, "acme").await;
    put_env(
        &api,
        "/api/workspaces/acme/env/OWN_TOKEN",
        secret_body(Some(REAL), &["o.example.org"]),
    )
    .await;
    put_env(
        &api,
        "/api/workspaces/acme/env/EDITOR",
        json!({"kind": "plain", "value": "nano"}),
    )
    .await;
    host.store()
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&puddle_store::NewRule {
            scope: puddle_store::Scope::Workspace(name("acme")),
            pattern: puddle_store::Pattern::parse("example.org").unwrap(),
            effect: puddle_store::Effect::Allow,
            expires_at: None,
            created_by: puddle_store::Actor::Cli,
        })
        .unwrap();
    assert_eq!(rig.secrets.ids().len(), 2);

    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let reply = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;

    // The workspace's own secret is gone from the credential store, the global one stays.
    eventually("the workspace's value is removed", APPLY, || {
        rig.secrets.ids().len() == 1
    })
    .await;
    assert!(
        host.store()
            .env_entries(
                puddle_store::UserId::OWNER,
                &puddle_store::EnvScope::Workspace(name("acme"))
            )
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        host.store()
            .env_entries(puddle_store::UserId::OWNER, &puddle_store::EnvScope::Global)
            .unwrap()
            .len(),
        1
    );
    assert!(
        host.store()
            .rules_every_user(&puddle_store::testing::all_users())
            .is_empty(),
        "its rules went with it"
    );

    // A new workspace of the same name (another id) starts clean, with the global secret only.
    create_as(&api, &mut events, "acme", "acme-2").await;
    let env = rig.guest.boot_envs.lock().unwrap().last().cloned().unwrap();
    assert!(env.get("OWN_TOKEN").is_none() && env.get("EDITOR").is_none());
    assert!(env.get("GLOBAL_TOKEN").is_some());
    host.shutdown().await;
}
