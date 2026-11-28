// SPDX-License-Identifier: AGPL-3.0-or-later
//! Terminals, event readers and the managed tools layer.

#[cfg(target_os = "linux")]
use super::secret_leaks::files_holding;
use super::*;

/// An event already in the socket when the deadline has passed is read, not reported as a timeout.
#[tokio::test]
async fn event_reader_sees_an_event_queued_past_the_deadline() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stream = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    let (mut server, _) = listener.accept().await.unwrap();
    server
        .write_all(b"data: {\"type\":\"ping\"}\n\n")
        .await
        .unwrap();
    stream.readable().await.unwrap();
    let mut events = support::Events::over(stream);
    let found = events.until(|e| e["type"] == "ping", Duration::ZERO).await;
    assert_eq!(found["type"], "ping");
}

async fn terminal_output_until(
    link: &mut puddle_services::terminals::TerminalLink,
    text: &str,
) -> puddle_services::terminals::TerminalEnd {
    let mut seen = String::new();
    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), link.output.recv())
            .await
            .unwrap();
        // A link that closes without an end fails the caller's assertion on the reason.
        let Some(item) = item else {
            return puddle_services::terminals::TerminalEnd::of(
                puddle_services::terminals::EndReason::Quitting,
            );
        };
        match item {
            puddle_services::terminals::TerminalOutput::Bytes(b) => {
                seen.push_str(&String::from_utf8_lossy(&b));
                if seen.contains(text) {
                    return puddle_services::terminals::TerminalEnd::of(
                        puddle_services::terminals::EndReason::Exited,
                    );
                }
            }
            puddle_services::terminals::TerminalOutput::Ended(end) => return end,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_runs_as_the_session_user_and_a_stop_ends_it_with_the_stopped_reason() {
    let rig = Rig::named_ids();
    rig.runtime.serve_terminals();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let user = api.get("/api/workspaces/acme").await.json()["session_user"]
        .as_str()
        .unwrap()
        .to_owned();
    let id = puddle_services::owned::OwnedWorkspace::for_test(
        puddle_store::UserId::OWNER,
        &WorkspaceId::new("acme").unwrap(),
    );
    let size = puddle_services::terminals::TerminalSize::new(80, 24).unwrap();

    let service = host.workspaces();
    puddle_services::terminals::TerminalService::check(service, &id)
        .await
        .unwrap();
    let mut link = puddle_services::terminals::TerminalService::open(service, &id, size)
        .await
        .unwrap();
    link.input
        .send(puddle_services::terminals::TerminalInput::Bytes(
            b"whoami\r".to_vec(),
        ))
        .await
        .unwrap();
    let reached = terminal_output_until(&mut link, &format!("{user}\r\n")).await;
    assert_eq!(
        reached.reason,
        puddle_services::terminals::EndReason::Exited,
        "{reached:?}"
    );
    assert_eq!(service.open_terminals(), 1, "the shell counts as open");

    let stop = api.post("/api/workspaces/acme/stop", "").await;
    assert_eq!(stop.status, 202, "{}", stop.body);
    let end = terminal_output_until(&mut link, "never printed").await;
    assert_eq!(
        end.reason,
        puddle_services::terminals::EndReason::Stopped,
        "{end:?}"
    );
    events.until(ended("acme"), Duration::from_secs(20)).await;
    eventually(
        "a stop leaves no terminal open",
        Duration::from_secs(10),
        || service.open_terminals() == 0,
    )
    .await;

    // A stopped workspace has no terminal to open.
    let refused = puddle_services::terminals::TerminalService::open(service, &id, size)
        .await
        .unwrap_err();
    assert!(
        matches!(
            refused,
            puddle_services::terminals::TerminalError::NotRunning(_)
        ),
        "{refused:?}"
    );
    assert!(matches!(
        puddle_services::terminals::TerminalService::check(service, &id).await,
        Err(puddle_services::terminals::TerminalError::NotRunning(_))
    ));
    host.shutdown().await;
}

/// Opens a terminal in `workspace`, asks the stand-in shell which sandbox serves it, and closes it.
async fn sandbox_a_terminal_reaches(
    service: &puddle_host::HostWorkspaces<FakeRuntime>,
    workspace: &str,
) -> String {
    let id = puddle_services::owned::OwnedWorkspace::for_test(
        puddle_store::UserId::OWNER,
        &WorkspaceId::new(workspace).unwrap(),
    );
    let size = puddle_services::terminals::TerminalSize::new(80, 24).unwrap();
    let mut link = puddle_services::terminals::TerminalService::open(service, &id, size)
        .await
        .unwrap();
    link.input
        .send(puddle_services::terminals::TerminalInput::Bytes(
            b"hostname\r".to_vec(),
        ))
        .await
        .unwrap();
    let mut seen = String::new();
    loop {
        let item = tokio::time::timeout(Duration::from_secs(10), link.output.recv())
            .await
            .unwrap();
        // A terminal that closes early returns what it said, so the caller's assertion fails on it.
        let Some(item) = item else {
            return format!("closed after {seen:?}");
        };
        if let puddle_services::terminals::TerminalOutput::Bytes(b) = item {
            seen.push_str(&String::from_utf8_lossy(&b));
            // The shell echoes the typed line, then answers with the name and a new prompt.
            if let Some((_, answer)) = seen.split_once("hostname\r\n")
                && let Some((name, _)) = answer.split_once("\r\n")
            {
                return name.to_owned();
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_opened_for_one_of_two_running_workspaces_runs_in_that_workspaces_sandbox() {
    let rig = Rig::named_ids();
    rig.runtime.serve_terminals();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "alpha").await;
    create(&api, &mut events, "bravo").await;
    let service = host.workspaces();

    // Each name reaches its own sandbox, in either order and again, so a terminal that always
    // went to the first or the last workspace, or swapped the two, fails.
    let mut reached = Vec::new();
    for workspace in ["alpha", "bravo", "alpha", "bravo", "bravo", "alpha"] {
        reached.push((
            workspace,
            sandbox_a_terminal_reaches(service, workspace).await,
        ));
    }
    let sandbox_of = |workspace: &str| {
        let id = WorkspaceId::new(workspace).unwrap();
        rig.runtime
            .calls()
            .into_iter()
            .find_map(|c| {
                (c.op == Op::Create && c.target.as_deref().is_some_and(|t| t.contains(id.as_str())))
                    .then_some(c.target)
                    .flatten()
            })
            .expect("the workspace's sandbox was created")
    };
    let (alpha, bravo) = (sandbox_of("alpha"), sandbox_of("bravo"));
    assert_ne!(alpha, bravo);
    for (workspace, sandbox) in &reached {
        let want = if *workspace == "alpha" {
            &alpha
        } else {
            &bravo
        };
        assert_eq!(sandbox, want, "the terminal opened for {workspace}");
    }

    // The runtime saw the terminals' connections on the same two sandboxes and no other.
    let served: std::collections::BTreeSet<String> = rig
        .runtime
        .calls()
        .into_iter()
        .filter(|c| c.op == Op::ServeSsh)
        .filter_map(|c| c.target)
        .collect();
    assert!(served.is_superset(&[alpha, bravo].into()), "{served:?}");
    host.shutdown().await;
}

const MISE_SUDOERS: &str = "/etc/sudoers.d/puddle-mise";

/// The variables the latest start gave the sandbox, as (name, value).
fn last_boot_guest_env(rig: &Rig) -> GuestEnv {
    rig.guest.boot_envs.lock().unwrap().last().cloned().unwrap()
}

fn mise_names(env: &GuestEnv) -> Vec<String> {
    env.iter()
        .map(|(name, _)| name)
        .filter(|name| name.starts_with("MISE_"))
        .map(str::to_owned)
        .collect()
}

/// The global switch for managed tools, set before any workspace exists.
async fn global_mise(api: &Api, on: bool) {
    let reply = api
        .request(
            "PUT",
            "/api/settings",
            Some(&json!({"tools": {"mise": on}}).to_string()),
            true,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_with_managed_tools_on_sets_mise_the_shims_first_and_the_sudo_keep_list() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    global_mise(&api, true).await;
    create(&api, &mut events, "acme").await;
    let env = last_boot_guest_env(&rig);
    let dir = "/workspace/.puddle/mise";
    assert_eq!(
        env.get("MISE_DATA_DIR"),
        Some(format!("{dir}/data").as_str())
    );
    assert_eq!(
        env.get("MISE_CACHE_DIR"),
        Some(format!("{dir}/cache").as_str())
    );
    assert_eq!(
        env.get("MISE_STATE_DIR"),
        Some(format!("{dir}/state").as_str())
    );
    assert_eq!(env.get("MISE_CEILING_PATHS"), Some("/"));
    assert_eq!(
        env.get("MISE_TRUSTED_CONFIG_PATHS"),
        Some("/workspace"),
        "the repository's own code is trusted by default"
    );
    assert_eq!(env.get("MISE_DISABLE_UPDATE_WARNING"), Some("1"));
    assert_eq!(env.get("MISE_USE_VERSIONS_HOST_TRACK"), Some("0"));
    let path = env.get("PATH").unwrap();
    assert!(path.starts_with(&format!("{dir}/data/shims:")), "{path}");
    assert!(path.ends_with(&format!(":{dir}/bin")), "{path}");
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    let sudoers = pem_of(&files, MISE_SUDOERS);
    for name in mise_names(&env) {
        assert!(sudoers.contains(&name), "{name} in {sudoers}");
    }
    assert!(
        sudoers.contains(&format!("secure_path=\"{path}\"")),
        "{sudoers}"
    );
    // Login shells get the same PATH after Debian's /etc/profile reset it.
    assert!(
        pem_of(&files, "/etc/profile.d/00-puddle-path.sh").contains(path),
        "00-puddle-path.sh"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_trust_the_repository_code_path_is_not_set_and_an_existing_workspace_follows_at_its_next_start()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    global_mise(&api, true).await;
    create(&api, &mut events, "acme").await;
    assert!(
        last_boot_guest_env(&rig)
            .get("MISE_TRUSTED_CONFIG_PATHS")
            .is_some()
    );
    let reply = api
        .request(
            "PUT",
            "/api/settings/workspaces/acme",
            Some(&json!({"tools": {"mise": true, "repo_code": false}}).to_string()),
            true,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    restart(&api, &mut events, "acme").await;
    let env = last_boot_guest_env(&rig);
    assert_eq!(env.get("MISE_TRUSTED_CONFIG_PATHS"), None);
    assert_eq!(env.get("MISE_CEILING_PATHS"), Some("/"));
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!pem_of(&files, MISE_SUDOERS).contains("TRUSTED"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_managed_tools_off_no_mise_variable_file_or_path_change_reaches_the_guest() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    global_mise(&api, false).await;
    create(&api, &mut events, "acme").await;
    let env = last_boot_guest_env(&rig);
    assert_eq!(mise_names(&env), Vec::<String>::new());
    assert_eq!(env.get("PATH"), None, "the image's PATH stays as it is");
    let files = rig.guest.plan_files(rig.guest.boots() - 1);
    assert!(!files.contains_key(MISE_SUDOERS));
    // A workspace's own switch off does the same under a global switch that is on.
    global_mise(&api, true).await;
    let reply = api
        .request(
            "PUT",
            "/api/settings/workspaces/acme",
            Some(&json!({"tools": {"mise": false, "repo_code": true}}).to_string()),
            true,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    restart(&api, &mut events, "acme").await;
    assert_eq!(mise_names(&last_boot_guest_env(&rig)), Vec::<String>::new());
    // Switched on, the next start has it.
    let reply = api
        .request(
            "PUT",
            "/api/settings/workspaces/acme",
            Some(&json!({"tools": {"mise": true, "repo_code": true}}).to_string()),
            true,
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    restart(&api, &mut events, "acme").await;
    assert!(last_boot_guest_env(&rig).get("MISE_DATA_DIR").is_some());
    host.shutdown().await;
}

/// Applies boot plan `n`'s merged files to the files under `root`, as the boot hook does through
/// `puddle-agent merge-file`: a file the plan merges gets Puddle's block, one it dropped loses it.
fn apply_plan_merges(rig: &Rig, n: usize, root: &std::path::Path, earlier: &[&str]) {
    use puddle_agent::merge_file::{Action, Request, run};
    let merges = rig.guest.plan_merges(n);
    let state_dir = root.join("state");
    let on_disk = |guest: &str| root.join(guest.trim_start_matches('/'));
    for guest in earlier.iter().copied().filter(|g| !merges.contains_key(*g)) {
        let request = Request {
            action: Action::Remove,
            state_dir: state_dir.clone(),
            key: guest.to_owned(),
            file: on_disk(guest),
        };
        run(&request, std::io::empty()).unwrap();
    }
    for (guest, spec) in &merges {
        let request = Request {
            action: Action::Apply { mode: 0o644 },
            state_dir: state_dir.clone(),
            key: guest.clone(),
            file: on_disk(guest),
        };
        run(&request, spec.as_slice()).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn turning_managed_tools_off_for_a_workspace_that_ran_with_them_takes_mise_out_at_its_next_start()
 {
    const SYSTEM_RC: [&str; 2] = ["/etc/bash.bashrc", "/etc/zsh/zshenv"];
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    global_mise(&api, true).await;
    create(&api, &mut events, "acme").await;
    let on_boot = rig.guest.boots() - 1;

    // The guest's own files: the image's two system rc files and what the user keeps in the home.
    let root = tempfile::tempdir().unwrap();
    let image_rc = "# the image's own line\n";
    let user_files = [
        (
            "home/agent/.bashrc",
            "alias ll='ls -l'\nexport MISE_MINE=1\n",
        ),
        (
            "home/agent/.config/mise/config.toml",
            "[tools]\nnode = \"20\"\n",
        ),
    ];
    for guest in SYSTEM_RC {
        let path = root.path().join(guest.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, image_rc).unwrap();
    }
    for (path, text) in user_files {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let read = |guest: &str| {
        std::fs::read_to_string(root.path().join(guest.trim_start_matches('/'))).unwrap()
    };

    // On: the plan has the variables, the shims first, the sudo file and the activate line.
    assert!(!mise_names(&last_boot_guest_env(&rig)).is_empty());
    let on_files = rig.guest.plan_files(on_boot);
    assert!(on_files.contains_key(MISE_SUDOERS));
    apply_plan_merges(&rig, on_boot, root.path(), &[]);
    for guest in SYSTEM_RC {
        let text = read(guest);
        assert!(text.contains("export MISE_DATA_DIR="), "{guest}: {text}");
        assert!(text.contains("mise activate"), "{guest}: {text}");
    }

    // Off, then the next start of the same workspace.
    global_mise(&api, false).await;
    restart(&api, &mut events, "acme").await;
    let off_boot = rig.guest.boots() - 1;
    assert!(off_boot > on_boot);
    let env = last_boot_guest_env(&rig);
    assert_eq!(mise_names(&env), Vec::<String>::new(), "no MISE_ variable");
    assert_eq!(env.get("PATH"), None, "no shims: the image's PATH stays");
    let off_files = rig.guest.plan_files(off_boot);
    assert!(
        !off_files.contains_key(MISE_SUDOERS),
        "the hook deletes a file the plan no longer has"
    );
    for (path, bytes) in &off_files {
        let text = String::from_utf8_lossy(bytes);
        assert!(!text.contains("MISE_"), "{path}: {text}");
        assert!(!text.contains("shims"), "{path}: {text}");
    }
    apply_plan_merges(&rig, off_boot, root.path(), &SYSTEM_RC);
    for guest in SYSTEM_RC {
        let text = read(guest);
        assert!(
            text.starts_with(image_rc),
            "{guest} keeps the image's line: {text}"
        );
        assert!(
            text.contains("export HTTPS_PROXY="),
            "{guest} keeps the other env: {text}"
        );
        assert!(!text.contains("MISE_"), "{guest}: {text}");
        assert!(!text.contains("mise"), "{guest}: {text}");
    }

    // Nothing the plan writes or merges is the user's own shell startup or mise configuration.
    let touched: Vec<String> = off_files
        .keys()
        .chain(rig.guest.plan_merges(off_boot).keys())
        .cloned()
        .collect();
    for path in &touched {
        assert!(
            !path.ends_with("/.bashrc")
                && !path.ends_with("/.profile")
                && !path.ends_with("/.zshrc")
                && !path.contains("/mise/")
                && !path.contains("/mise."),
            "{path} is the user's"
        );
    }
    for (path, text) in user_files {
        assert_eq!(
            std::fs::read_to_string(root.path().join(path)).unwrap(),
            text
        );
    }
    host.shutdown().await;
}

const MISE_FAILURE: &str = "could not download mise: curl failed (exit status: 56): 403";

fn tools_problem(
    problems: &[(String, Option<String>, String, String)],
) -> Option<&(String, Option<String>, String, String)> {
    problems.iter().find(|p| p.0 == "tools:acme")
}

#[tokio::test(flavor = "multi_thread")]
async fn mise_is_fetched_in_the_background_after_the_start_and_a_good_fetch_raises_nothing() {
    let rig = Rig::named_ids();
    rig.mise_on.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    eventually("the guest is asked for mise", APPLY, || {
        !mise_fetches(&rig).is_empty()
    })
    .await;
    let fetches = mise_fetches(&rig);
    assert_eq!(fetches.len(), 1, "{fetches:?}");
    assert_eq!(fetches[0].0, ["mise-fetch", "/workspace/.puddle/mise"]);
    assert_eq!(
        fetches[0].1.as_deref(),
        Some("agent"),
        "as the session user"
    );
    assert_eq!(tools_problem(&problems_of(&api).await), None);
    // Every start asks again: the agent finds the binary current and does nothing.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    eventually("the second start asks too", APPLY, || {
        mise_fetches(&rig).len() == 2
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_mise_fetch_is_a_problem_with_the_blocking_rule_and_the_start_still_ends_done() {
    let rig = Rig::named_ids();
    rig.mise_on.store(true, Ordering::SeqCst);
    *rig.guest.mise_failure.lock().unwrap() = Some(MISE_FAILURE);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    puddle_store::testing::own(host.store(), &name("acme"));
    host.store()
        .as_user(puddle_store::UserId::OWNER)
        .add_rule(&puddle_store::NewRule {
            scope: puddle_store::Scope::Workspace(name("acme")),
            pattern: puddle_store::Pattern::parse("mise.jdx.dev").unwrap(),
            effect: puddle_store::Effect::Deny,
            expires_at: None,
            created_by: puddle_store::Actor::Cli,
        })
        .unwrap();
    create(&api, &mut events, "acme").await;
    assert_eq!(status_of(&api.get("/api/workspaces/acme").await), "running");
    eventually("the problem is raised", APPLY, || {
        // Polled on the host's own state: the API call below is async.
        host.problems().list().iter().any(|p| p.key == "tools:acme")
    })
    .await;
    let problems = problems_of(&api).await;
    let (_, workspace, title, detail) = tools_problem(&problems).unwrap();
    assert_eq!(workspace.as_deref(), Some("acme"));
    assert!(title.contains("mise"), "{title}");
    assert!(detail.contains(MISE_FAILURE), "{detail}");
    assert!(detail.contains("works without them"), "{detail}");
    assert!(
        detail.contains("deny mise.jdx.dev"),
        "names the rule: {detail}"
    );
    // The next start finds the download working and the problem ends.
    *rig.guest.mise_failure.lock().unwrap() = None;
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    eventually("the problem ends", APPLY, || {
        !host.problems().list().iter().any(|p| p.key == "tools:acme")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_mise_fetch_without_an_own_rule_names_no_rule() {
    let rig = Rig::named_ids();
    rig.mise_on.store(true, Ordering::SeqCst);
    *rig.guest.mise_failure.lock().unwrap() = Some(MISE_FAILURE);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    eventually("the problem is raised", APPLY, || {
        host.problems().list().iter().any(|p| p.key == "tools:acme")
    })
    .await;
    let problems = problems_of(&api).await;
    let (_, _, _, detail) = tools_problem(&problems).unwrap();
    assert!(!detail.contains("A rule you made"), "{detail}");
}

#[tokio::test(flavor = "multi_thread")]
async fn with_mise_off_nothing_is_fetched_and_an_old_problem_goes() {
    for switch in ["global", "workspace"] {
        let rig = Rig::named_ids();
        rig.mise_on.store(true, Ordering::SeqCst);
        *rig.guest.mise_failure.lock().unwrap() = Some(MISE_FAILURE);
        let host = rig.start().await;
        let api = api(&host);
        let mut events = api.events().await;
        create(&api, &mut events, "acme").await;
        eventually("the problem is raised", APPLY, || {
            host.problems().list().iter().any(|p| p.key == "tools:acme")
        })
        .await;
        let before = mise_fetches(&rig).len();
        let reply = if switch == "global" {
            api.put(
                "/api/settings",
                &json!({"tools": {"mise": false}}).to_string(),
            )
            .await
        } else {
            api.put(
                "/api/settings/workspaces/acme",
                &json!({"tools": {"mise": false}}).to_string(),
            )
            .await
        };
        assert_eq!(reply.status, 200, "{switch}: {}", reply.body);
        api.post("/api/workspaces/acme/stop", "").await;
        events.until(ended("acme"), Duration::from_secs(20)).await;
        api.post("/api/workspaces/acme/start", "").await;
        events.until(ended("acme"), Duration::from_secs(20)).await;
        eventually("the old problem ends", APPLY, || {
            !host.problems().list().iter().any(|p| p.key == "tools:acme")
        })
        .await;
        assert_eq!(mise_fetches(&rig).len(), before, "{switch}: no new fetch");
    }
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn a_headless_host_keeps_an_environment_secret_a_pasted_token_and_a_proxy_sign_in_in_the_file_vault()
 {
    let rig = Rig::new();
    rig.file_vault.store(true, Ordering::SeqCst);
    let proxy = puddle_upstream::ProxyAddr::new("proxy.corp", 8080);
    let sign_in = fixture::password("vault");
    let token = "ghp_CANARY-pasted-token-in-the-file-vault";

    let host = rig.start().await;
    proxy_sign_ins_loaded(&host, 0).await;
    let first = api(&host);
    put_env(
        &first,
        "/api/env/GLOBAL_TOKEN",
        secret_body(Some(REAL), &["api.example.org"]),
    )
    .await;
    add_github_identity(&first, token).await;
    host.proxy_passwords().set(&proxy, "svc", &sign_in).unwrap();
    assert!(
        problems_of(&first).await.is_empty(),
        "no credential-store problem"
    );
    let record = std::fs::read_to_string(rig.dir.path().join("data/secrets/store.json")).unwrap();
    assert!(record.contains("\"store\": \"file\""), "{record}");
    host.shutdown().await;
    drop(host);

    // The values are in the vault file and nowhere else: not in the memory store the other tests
    // hand in, and no file of the run holds one in the clear.
    assert!(rig.secrets.ids().is_empty());
    for canary in [REAL, token, sign_in.as_str()] {
        assert_eq!(files_holding(rig.dir.path(), canary), Vec::<String>::new());
    }
    let vault_file = puddle_secrets::vault_path(&rig.dir.path().join("data"));
    assert!(vault_file.is_file());
    let held = open_the_vault(&rig, &vault_file).await;
    for canary in [REAL, token] {
        assert!(
            held.iter().any(|value| value == canary),
            "{canary} not in the vault"
        );
    }

    // A start without the flag reads the record, opens the same vault and finds the sign-in.
    rig.without_headless_flag.store(true, Ordering::SeqCst);
    let again = rig.start().await;
    proxy_sign_ins_loaded(&again, 1).await;
    assert_eq!(
        again.proxy_passwords().proxies(),
        [(proxy, "svc".to_owned())]
    );
    assert!(problems_of(&api(&again)).await.is_empty());
    again.shutdown().await;
    drop(again);

    // A vault whose key is gone does not stop the host: it starts, says why secrets cannot be
    // saved, and leaves the vault file as it was.
    let before = std::fs::read(&vault_file).unwrap();
    std::fs::remove_file(rig.dir.path().join("config/puddle/vault.key")).unwrap();
    let keyless = rig.start().await;
    let problems = problems_of(&api(&keyless)).await;
    assert!(
        problems.iter().any(|p| p.0 == "credential-store"),
        "{problems:?}"
    );
    assert_eq!(std::fs::read(&vault_file).unwrap(), before);
    keyless.shutdown().await;
}

#[cfg(target_os = "linux")]
/// Every value in the vault at `file`, opened with the key the rig's host kept in its config
/// folder: what a restart of the host would read.
async fn open_the_vault(rig: &Rig, file: &std::path::Path) -> Vec<String> {
    let keys = puddle_secrets::VaultKeys::new(
        &rig.dir.path().join("config"),
        file,
        puddle_secrets::ToolPaths::default(),
    );
    let found = keys.find_or_make().await.unwrap();
    let vault = puddle_secrets::FileVault::open(file, found.key).unwrap();
    vault
        .ids()
        .unwrap()
        .iter()
        .filter_map(|id| {
            vault
                .get(&puddle_secrets::StoredId::new(id.as_str()).unwrap())
                .unwrap()
        })
        .map(|secret| secret.expose().to_owned())
        .collect()
}
