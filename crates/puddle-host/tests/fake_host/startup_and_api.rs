// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starting the host, serving the API, and the system checks.

use super::*;

#[cfg(target_os = "linux")]
#[test]
fn a_tests_data_folder_is_in_memory_where_the_machine_has_a_memory_mount() {
    let dir = support::scratch_dir();
    match std::env::var_os("TMPDIR") {
        // A `TMPDIR` the runner set is respected, whether or not it is itself in RAM (the check
        // host puts it under `/dev/shm`).
        Some(tmp) => assert!(dir.path().starts_with(&tmp), "{:?}", dir.path()),
        None => assert_eq!(
            dir.path().starts_with("/dev/shm"),
            support::is_ram_mount("/dev/shm"),
            "{:?}",
            dir.path()
        ),
    }
    assert!(!support::is_ram_mount("/"), "the root is no memory mount");
    assert!(!support::is_ram_mount("/no/such/mount"));
    // Whatever the folder is, a host can write and sync a file in it.
    let file = std::fs::File::create(dir.path().join("probe")).unwrap();
    file.sync_all().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn start_runs_the_steps_in_the_documented_order() {
    let rig = Rig::new();
    let platform = FakePlatform::new(&rig.log);
    let prepared = prepare(rig.config(), &platform).unwrap();
    assert_eq!(prepared.steps(), PREPARE_STEPS);
    let factory = FakeFactory::new(&rig.runtime, &rig.log);
    let host = Host::start(prepared, &factory, hermetic_options())
        .await
        .unwrap();
    assert_eq!(host.steps(), START_STEPS);
    // The machine was asked in dependency order and the runtime opened last of those.
    assert_eq!(
        rig.calls(),
        ["check_runtime", "corporate_roots", "open_runtime"]
    );
    // The runtime got the address of the pull proxy bound in the first step, loopback only,
    // and no roots (this machine has none).
    let proxy = factory.pull_proxy.lock().unwrap().clone().unwrap();
    assert!(proxy.starts_with("127.0.0.1:"), "{proxy}");
    assert_eq!(*factory.roots.lock().unwrap(), Some(0));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_step_stops_the_start_before_the_runtime_is_touched() {
    for refused in ["check_runtime", "corporate_roots"] {
        let rig = Rig::new();
        let mut platform = FakePlatform::new(&rig.log);
        platform.refuse = Some(refused);
        let err = prepare(rig.config(), &platform).unwrap_err();
        assert!(err.to_string().contains(refused), "{err}");
        assert!(!rig.calls().contains(&"open_runtime".to_owned()));
        // Nothing was asked of the runtime.
        assert!(rig.runtime.calls().is_empty());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_agent_stops_the_start_before_anything_is_reconciled() {
    let rig = Rig::new();
    let mut config = rig.config();
    config.guest = GuestSettings::new(rig.dir.path().join("no-agent"));
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::AgentMissing { .. }), "{err}");
    assert!(!rig.calls().contains(&"open_runtime".to_owned()));
    assert!(rig.runtime.calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_serving_gives_the_host_a_network_listener_with_its_own_certificate_only_when_asked()
{
    use std::net::{IpAddr, Ipv4Addr};
    let serving = || {
        puddle_api::RemoteServing::new(
            vec![puddle_api::PublicHost::parse("puddle.example.org").unwrap()],
            None,
        )
        .unwrap()
    };
    let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);

    let rig = Rig::new();
    let mut config = rig.config();
    config.api.remote = Some(puddle_host::RemoteSettings {
        serving: serving(),
        network: None,
    });
    let host = rig.start_with(config).await;
    assert_eq!(
        host.remote_listener(),
        None,
        "a front on this machine needs none"
    );
    host.shutdown().await;

    let rig = Rig::new();
    let mut config = rig.config();
    config.api.remote = Some(puddle_host::RemoteSettings {
        serving: serving(),
        network: Some(puddle_api::NetworkEndpoint::new(loopback, 0, vec![loopback]).unwrap()),
    });
    let host = rig.start_with(config).await;
    let listener = host.remote_listener().expect("a network listener").clone();
    assert_eq!(listener.addr.ip(), loopback);
    assert_eq!(listener.fingerprint.len(), 64);
    assert!(
        listener.certificate_file.is_file(),
        "{:?}",
        listener.certificate_file
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_taken_api_port_is_reported_and_nothing_was_stopped_or_removed() {
    let rig = Rig::new();
    let first = rig.start().await;
    // Another data folder, so only the port is shared.
    let mut config = rig.config();
    config.paths = HostPaths::new(rig.dir.path().join("other-data"));
    config.api.port = first.url().port().unwrap();
    let prepared = prepare(config, &FakePlatform::new(&rig.log)).unwrap();
    let err = Host::start(
        prepared,
        &FakeFactory::new(&rig.runtime, &rig.log),
        hermetic_options(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, HostError::Api(_)), "{err}");
    first.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_host_on_the_same_data_folder_is_refused_and_the_first_is_untouched() {
    let rig = Rig::named_ids();
    let first = rig.start().await;
    let api = api(&first);
    let mut events = api.events().await;
    create(&api, &mut events, "keep").await;
    after_boot_listing(&rig).await;
    let running = rig
        .runtime
        .list()
        .await
        .unwrap()
        .iter()
        .filter(|s| !s.status.is_down())
        .count();
    assert_eq!(running, 1);
    let calls_before = rig.runtime.calls();
    let log_before = rig.calls();

    let err = prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap_err();
    assert!(matches!(err, HostError::DataFolder(_)), "{err}");
    let message = err.to_string();
    assert!(
        message.contains(&format!("process ID {}", std::process::id())),
        "{message}"
    );
    assert!(message.contains("data folder"), "{message}");

    // The refused start asked the machine nothing and the runtime nothing: the first host's
    // sandbox was neither trimmed nor stopped.
    assert_eq!(rig.calls(), log_before);
    assert_eq!(rig.runtime.calls(), calls_before);
    let still = rig.runtime.list().await.unwrap();
    assert_eq!(still.iter().filter(|s| !s.status.is_down()).count(), 1);
    assert_eq!(api.get("/api/health").await.status, 200);

    // Once the first host is gone the folder is free again.
    first.shutdown().await;
    drop(first);
    drop(prepare(rig.config(), &FakePlatform::new(&rig.log)).unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daemon_serves_the_api_the_ui_and_the_event_stream() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);

    // The API wants the token; the UI is served from the same origin without one.
    assert_eq!(
        api.request("GET", "/api/health", None, false).await.status,
        401
    );
    assert_eq!(api.get("/api/health").await.status, 200);
    let ui = api.request("GET", "/", None, false).await;
    assert_eq!(ui.status, 200);
    assert!(ui.body.contains("Puddle test ui"), "{}", ui.body);

    // The connection file names this host.
    let info =
        puddle_api::ConnectionInfo::read(&rig.dir.path().join("data").join("api.json")).unwrap();
    assert_eq!(info.token.expose(), host.token());

    // Create: events flow, in order, from the real service.
    let mut events = api.events().await;
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 202, "{}", reply.body);
    assert_eq!(reply.json()["busy"], "creating");
    let end = events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(end["step"], "done", "{end}");
    assert_eq!(
        events.steps("acme"),
        [
            "preparing_volume",
            "pulling_image",
            "starting",
            "waiting_for_agent",
            "syncing",
            "cloning",
            "done"
        ]
    );
    assert!(
        events
            .seen
            .iter()
            .any(|e| e["type"] == "status_changed" && e["status"] == "running")
    );

    let list = api.get("/api/workspaces").await.json();
    assert_eq!(list["workspaces"][0]["name"], "acme");
    assert_eq!(list["workspaces"][0]["status"], "running");
    assert!(list["workspaces"][0]["busy"].is_null());

    // The guest saw the whole boot sequence, in order.
    let commands = rig.guest.commands();
    let kinds: Vec<&str> = commands
        .iter()
        .map(|c| c.split(' ').nth(1).unwrap())
        .collect();
    assert_eq!(kinds, ["boot", "layout", "locks", "clone", "sync"]);

    // Stop trims then stops; start boots it again in the same sandbox.
    let stop = api.post("/api/workspaces/acme/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "stopped");
    let start = api.post("/api/workspaces/acme/start", "").await;
    assert_eq!(start.status, 202, "{}", start.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "running");
    let calls: Vec<String> = rig
        .runtime
        .calls()
        .iter()
        .map(|c| format!("{:?}", c.op))
        .collect();
    assert_eq!(
        calls.iter().filter(|c| *c == "Create").count(),
        1,
        "{calls:?}"
    );
    assert_eq!(
        calls.iter().filter(|c| *c == "Start").count(),
        1,
        "{calls:?}"
    );

    // Stop, then delete a clean workspace without a fingerprint.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let gone = api
        .delete(
            "/api/workspaces/acme",
            Some(&json!({"confirm": true}).to_string()),
        )
        .await;
    assert_eq!(gone.status, 202, "{}", gone.body);
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let list = api.get("/api/workspaces").await.json();
    assert!(list["workspaces"].as_array().unwrap().is_empty());
    assert!(rig.runtime.list().await.unwrap().is_empty());
    assert!(rig.runtime.list_volumes().await.unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_apis_address_is_registered_so_no_guest_can_ask_the_proxy_for_it() {
    let rig = Rig::new();
    let host = rig.start().await;
    let addr = std::net::SocketAddr::new(
        host.url().host_str().unwrap().parse().unwrap(),
        host.url().port().unwrap(),
    );
    let kinds: Vec<_> = host
        .endpoints()
        .list()
        .into_iter()
        .map(|(a, kind)| (a == addr, kind))
        .collect();
    assert!(kinds.contains(&(true, EndpointKind::Api)), "{kinds:?}");
    // The pull proxy is registered too (the other loopback entry).
    assert!(
        kinds.iter().any(|(_, k)| *k == EndpointKind::PullProxy),
        "{kinds:?}"
    );
    host.shutdown().await;
    assert!(
        host.endpoints().list().is_empty(),
        "{:?}",
        host.endpoints().list()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ending_the_extension_hosts_of_a_workspace_that_has_no_agent_says_it_is_not_running() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceId::new("hosts").unwrap();
    let not_running = |result: Result<_, puddle_host::EndExtensionHostsError>| {
        assert!(
            matches!(result, Err(puddle_host::EndExtensionHostsError::NotRunning)),
            "{result:?}"
        );
    };
    // Unknown, then known but without a connected agent (the fake guest runs none): nothing to
    // reach, and no hang.
    not_running(host.workspaces().end_extension_hosts(&name).await);
    create(&api, &mut events, "hosts").await;
    not_running(host.workspaces().end_extension_hosts(&name).await);
    api.post("/api/workspaces/hosts/stop", "").await;
    events.until(ended("hosts"), Duration::from_secs(20)).await;
    not_running(host.workspaces().end_extension_hosts(&name).await);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_an_editor_build_needs_no_connected_window_and_a_running_agent() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceId::new("builds").unwrap();
    // A window on the server: refused before anything is asked of the guest, even for a
    // workspace that does not exist.
    let refused = host.workspaces().switch_editor_build(&name, 2).await;
    assert!(
        matches!(
            refused,
            Err(puddle_host::SwitchEditorBuildError::WindowsConnected(2))
        ),
        "{refused:?}"
    );
    // No window, but no agent either (unknown, then known with a fake guest that runs none).
    let not_running = |result: Result<bool, puddle_host::SwitchEditorBuildError>| {
        assert!(
            matches!(result, Err(puddle_host::SwitchEditorBuildError::NotRunning)),
            "{result:?}"
        );
    };
    not_running(host.workspaces().switch_editor_build(&name, 0).await);
    create(&api, &mut events, "builds").await;
    not_running(host.workspaces().switch_editor_build(&name, 0).await);
    host.shutdown().await;
}

/// Waits until the guest agent `editor_stand_in` started has said hello on `name`'s route, so the
/// host holds a link to it.
async fn agent_linked(host: &Host<FakeRuntime>, name: &WorkspaceId) {
    puddle_test_kit::eventually_async("the guest agent connected", Duration::from_secs(20), || {
        host.workspaces().end_extension_hosts(name)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connected_agent_answers_the_end_of_the_extension_hosts_and_a_build_switch() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "linked").await;
    let name = WorkspaceId::new("linked").unwrap();
    let _editor = editor_stand_in(&rig, "linked").await;
    agent_linked(&host, &name).await;
    let ending = host.workspaces().end_extension_hosts(&name).await;
    assert!(ending.is_ok(), "{ending:?}");
    // The agent has staged no newer build, so it switches nothing and says so.
    let switched = host.workspaces().switch_editor_build(&name, 0).await;
    assert!(matches!(switched, Ok(false)), "{switched:?}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_workspace_that_is_not_running_cannot_be_stopped_or_attached_to_and_an_unknown_one_has_no_delete_check()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "idle").await;
    api.post("/api/workspaces/idle/stop", "").await;
    events.until(ended("idle"), Duration::from_secs(20)).await;
    let again = api.post("/api/workspaces/idle/stop", "").await;
    assert_eq!(again.status, 409, "{}", again.body);
    assert!(
        again.json()["message"]
            .as_str()
            .unwrap()
            .ends_with("is not running (stopped)"),
        "{}",
        again.body
    );
    // So does the check that runs before a delete, for a workspace the host does not know.
    let unknown = host
        .workspaces()
        .delete_check(&WorkspaceId::new("nobody").unwrap())
        .await;
    assert!(
        matches!(
            unknown,
            Err(puddle_services::workspaces::WorkspaceError::NotFound(_))
        ),
        "{unknown:?}"
    );
    // The host refuses an attach to it as well, for a caller that did not pass the API's checks.
    let attached = host
        .workspaces()
        .attach(
            &WorkspaceId::new("idle").unwrap(),
            puddle_services::workspaces::AttachMode::Browser,
        )
        .await;
    assert!(
        matches!(
            attached,
            Err(puddle_services::workspaces::WorkspaceError::Conflict(ref m)) if m.ends_with("is not running; start it first")
        ),
        "{attached:?}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_workspace_service_prints_how_many_workspaces_it_holds() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    assert_eq!(
        format!("{:?}", host.workspaces()),
        "HostWorkspaces { workspaces: 0, .. }"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_round_of_build_switches_asks_only_microsoft_workspaces_without_a_window() {
    let rig = Rig::named_ids();
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let windows = puddle_front::Windows::default();
    let workspaces = host.workspaces();
    assert_eq!(workspaces.switch_idle_editor_builds(&windows).await, 0);
    create(&api, &mut events, "ms").await;
    assert_eq!(workspaces.switch_idle_editor_builds(&windows).await, 1);
    let window = windows.open(&WorkspaceId::new("ms").unwrap());
    assert_eq!(workspaces.switch_idle_editor_builds(&windows).await, 0);
    drop(window);
    api.post("/api/workspaces/ms/stop", "").await;
    events.until(ended("ms"), Duration::from_secs(20)).await;
    assert_eq!(workspaces.switch_idle_editor_builds(&windows).await, 0);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_api_and_the_pull_proxy_are_each_registered_once_in_the_one_registry() {
    let rig = Rig::new();
    let host = rig.start().await;
    let kinds: Vec<_> = host
        .endpoints()
        .list()
        .into_iter()
        .map(|(_, kind)| kind)
        .collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == EndpointKind::Api).count(),
        1,
        "{kinds:?}"
    );
    assert_eq!(
        kinds
            .iter()
            .filter(|k| **k == EndpointKind::PullProxy)
            .count(),
        1,
        "{kinds:?}"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_network_health_report_is_the_hosts_own_and_follows_a_network_change() {
    let rig = Rig::new();
    let os = FakeOs::new(ProxyConfig::default());
    let mut platform = FakePlatform::new(&rig.log);
    platform.roots = company_roots();
    let prepared = prepare(rig.config(), &platform).unwrap();
    let mut options = hermetic_options();
    options.discovery = Some(Discovery::new(
        os.clone(),
        puddle_upstream::Config::default(),
    ));
    let host = Host::start(prepared, &FakeFactory::new(&rig.runtime, &rig.log), options)
        .await
        .unwrap();
    let api = api(&host);

    assert_eq!(
        api.request("GET", "/api/network-health", None, false)
            .await
            .status,
        401
    );
    let reply = api.get("/api/network-health").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let report = reply.json();
    assert_eq!(report["roots"]["synced"], true, "{report}");
    assert_eq!(report["roots"]["roots"], 1, "{report}");
    assert_eq!(
        report["roots"]["certificates"][0]["subject"],
        "Corp Root CA"
    );
    assert_eq!(
        report["pull_proxy"],
        json!({"active": true, "via_upstream": true})
    );
    assert_eq!(report["proxy"]["epoch"], 0);
    assert_eq!(report["proxy"]["last_change_at"], serde_json::Value::Null);

    // A network change: the event, then a report that shows the new epoch.
    let mut events = api.events().await;
    assert!(os.fire_change());
    let event = events
        .until(|e| e["type"] == "network_changed", Duration::from_secs(20))
        .await;
    assert_eq!(event["epoch"], 1, "{event}");
    let after = api.get("/api/network-health").await.json();
    assert_eq!(after["proxy"]["epoch"], 1, "{after}");
    assert!(after["proxy"]["last_change_at"].as_u64().unwrap() > 0);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_repository_lists_run_on_the_real_engine_and_a_host_it_does_not_know_is_asked_nothing()
{
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);

    // Wired: no identities is an empty answer, not a 503.
    let none = api.get("/api/repos").await;
    assert_eq!(none.status, 200, "{}", none.body);
    assert_eq!(
        none.json(),
        json!({"sources": [], "repos": [], "total": 0, "offset": 0, "limit": 100})
    );

    // A credential for a host that is neither GitHub nor Azure DevOps: the engine says so and
    // sends nothing (no token is read, no connection is made).
    let made = api
        .post(
            "/api/identities",
            &json!({
                "label": "Other",
                "author": {"name": "Other", "email": "other@example.org"},
                "credentials": [{
                    "host": "git.example.org",
                    "source": {"kind": "stored", "id": "tok-1", "host": "git.example.org", "org": null},
                    "covers": {"owners": [], "rest_of_host": true}
                }]
            })
            .to_string(),
        )
        .await;
    assert_eq!(made.status, 201, "{}", made.body);
    let listing = api.get("/api/repos").await.json();
    assert_eq!(listing["sources"][0]["state"], "unavailable", "{listing}");
    assert_eq!(listing["sources"][0]["problem"]["code"], "unsupported");
    assert_eq!(listing["total"], 0);

    let profile = api
        .post(
            "/api/credentials/profile",
            &json!({"source": {"kind": "stored", "id": "tok-1", "host": "git.example.org", "org": null}})
                .to_string(),
        )
        .await;
    assert_eq!(profile.status, 200, "{}", profile.body);
    assert_eq!(profile.json()["problem"]["code"], "unsupported");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_system_check_looks_at_the_runtime_folder_the_host_was_started_with() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);

    assert_eq!(
        api.request("GET", "/api/doctor?boot=false", None, false)
            .await
            .status,
        401
    );
    // The rig's runtime folder holds no msb: the check says so, names the folder, and skips what
    // needs a runtime. (The machine's own hypervisor check can come out either way.)
    let reply = api.get("/api/doctor?boot=false").await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let report = reply.json();
    assert_eq!(report["schema_version"], 1, "{report}");
    assert_eq!(report["ok"], false, "{report}");
    let check = |id: &str| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("no {id} check in {report}"))
            .clone()
    };
    let runtime = check("runtime");
    assert_eq!(runtime["status"], "fail", "{runtime}");
    assert_eq!(runtime["finding"], "runtime_missing", "{runtime}");
    assert!(
        runtime["summary"]
            .as_str()
            .unwrap()
            .contains(&rig.dir.path().join("runtime").display().to_string()),
        "{runtime}"
    );
    assert_eq!(check("launch")["status"], "skipped");
    assert_eq!(check("test_boot")["status"], "skipped");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ssh_agent_named_in_the_settings_is_the_one_the_system_check_asks() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    let agent_line = |report: serde_json::Value| {
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "ssh_agent")
            .unwrap_or_else(|| panic!("no ssh_agent check in {report}"))
            .to_string()
    };
    let named = rig.dir.path().join("my-agent").display().to_string();
    let before = agent_line(api.get("/api/doctor?boot=false").await.json());
    assert!(!before.contains("my-agent"), "{before}");

    let body = json!({"ui": {"ssh_agent_pipe": format!("  {named}  ")}});
    let put = api.put("/api/settings", &body.to_string()).await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(put.json()["ui"]["ssh_agent_pipe"], named);
    let after = agent_line(api.get("/api/doctor?boot=false").await.json());
    // The line is JSON text, so a Windows path's backslashes show doubled.
    let escaped = json!(named).to_string();
    let escaped = escaped.trim_matches('"');
    assert!(after.contains(escaped), "the doctor asks {named}: {after}");

    let cleared = json!({"ui": {"ssh_agent_pipe": "  "}});
    assert_eq!(
        api.put("/api/settings", &cleared.to_string()).await.status,
        200
    );
    let again = agent_line(api.get("/api/doctor?boot=false").await.json());
    assert!(!again.contains("my-agent"), "{again}");
    host.shutdown().await;
}
