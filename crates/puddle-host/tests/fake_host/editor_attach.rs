// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attaching the desktop and browser editors, and the editor data a delete or create clears.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn attach_opens_the_desktop_editor_through_the_launcher() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let before = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(before.status, 404);
    create(&api, &mut events, "acme").await;
    let refused = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(refused.status, 409, "direct SSH is off: {}", refused.body);
    assert_eq!(
        refused.json()["message"],
        "direct SSH is off for this workspace; allow it first"
    );
    // The host refuses it itself too, for a caller that did not pass the API's check.
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
    assert_eq!(
        attached.message.as_deref(),
        Some("direct SSH is off for this workspace; allow it first")
    );
    assert!(rig.launcher.opened.lock().unwrap().is_empty());
    allow_direct_ssh(&api, "acme", true).await;
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.json()["opened"], true);
    let uri = reply.json()["open_uri"].as_str().unwrap().to_owned();
    assert!(
        uri.starts_with("vscode://vscode-remote/ssh-remote+puddle-acme/"),
        "{uri}"
    );
    assert_eq!(*rig.launcher.opened.lock().unwrap(), ["acme"]);
    let browser = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(browser.json()["opened"], true);
    assert_eq!(rig.launcher.browser.lock().unwrap().len(), 1);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_hands_the_launcher_the_fronts_origin_and_the_front_carries_a_request_to_the_editor()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let editor = editor_stand_in(&rig, "acme").await;

    let origin = browser_origin(&rig, &api, "acme").await;
    let front_port: u16 = origin
        .strip_prefix("http://acme.localhost:")
        .unwrap_or_else(|| panic!("not the workspace's origin: {origin}"))
        .parse()
        .unwrap();
    assert!(
        host.endpoints()
            .list()
            .iter()
            .any(|(addr, kind)| addr.port() == front_port && *kind == EndpointKind::BrowserFront),
        "the origin's port is the host's front, registered as Puddle's own listener"
    );

    // The agent says hello after the attach asked: wait for the front to find it.
    let answer = served_at(&origin).await;
    assert!(answer.ends_with("hello"), "{answer}");
    assert_eq!(editor.accepted.load(Ordering::SeqCst), 1);

    // A stopped workspace is not served: the front looks at the running workspaces on every request.
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let gone = window_gets(&origin).await;
    assert!(gone.starts_with("HTTP/1.1 404"), "{gone}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_gives_the_window_the_front_the_settings_and_a_way_to_the_password() {
    let rig = Rig::named_ids();
    write_global_settings(
        &rig,
        &json!({"schema_version": 2, "workspace_defaults": {"zoom_hotkeys": false}}),
    );
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let editor = editor_stand_in(&rig, "acme").await;

    let target = browser_target(&rig, &api, "acme").await;
    assert_eq!(target.front.port(), front_port_of(&host));
    assert!(target.front.ip().is_loopback());
    assert_eq!(
        target.origin,
        format!("http://acme.localhost:{}", target.front.port())
    );
    assert!(
        !target.settings.zoom_hotkeys.value,
        "the workspace's own setting reaches the window"
    );
    // No window has connected to the editor, so the front counts none.
    let acme = WorkspaceId::new("acme").unwrap();
    assert_eq!(host.editor_windows().connected(&acme), 0);
    // code-server asks for a password: the window reads it when it signs in, from the guest's file.
    // The agent may say hello after the window asked: the sign-in waits for it.
    let Some(puddle_services::workspaces::EditorSignIn::Form(login)) = target.login else {
        panic!("code-server signs in with its login form");
    };
    assert_eq!(login.password().await.unwrap(), "0".repeat(64));
    // An agent with no password yet, and a workspace that has stopped, are said in words.
    std::fs::remove_file(&editor.password_file).unwrap();
    let err = login.password().await.unwrap_err().to_string();
    assert!(err.contains("no password to sign in with yet"), "{err}");
    api.post("/api/workspaces/acme/stop", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let err = login.password().await.unwrap_err().to_string();
    assert_eq!(err, "acme is not running; start it first");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_cannot_sign_in_names_the_workspace_by_its_name_not_its_id() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_as(&api, &mut events, "Acme Web", "acme-web").await;
    let _editor = editor_stand_in(&rig, "acme-web").await;
    let target = browser_target(&rig, &api, "acme-web").await;
    let Some(puddle_services::EditorSignIn::Form(login)) = target.login else {
        panic!("code-server signs in with its login form");
    };
    api.post("/api/workspaces/acme-web/stop", "").await;
    events
        .until(ended("acme-web"), Duration::from_secs(20))
        .await;
    let err = login.password().await.unwrap_err().to_string();
    assert_eq!(err, "Acme Web is not running; start it first");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_signs_in_before_the_agents_hello_waits_for_it_and_gets_the_password() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    // The workspace reports running and no agent has said hello: the window signs in now.
    let target = browser_target(&rig, &api, "acme").await;
    let login = target
        .login
        .expect("code-server asks for a password")
        .reader()
        .clone();
    let asked = tokio::spawn(async move { login.password().await });
    // An absence window: nothing is there to signal that the sign-in has not given up.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!asked.is_finished(), "the sign-in waits, it does not fail");
    // The hello arrives late.
    let _editor = editor_stand_in(&rig, "acme").await;
    assert_eq!(asked.await.unwrap().unwrap(), "0".repeat(64));
    host.shutdown().await;
}

/// Creates `acme` while the settings are damaged (the workspace names its memory, so the damage
/// does not refuse the create), then attaches in a browser window; returns the reply body.
async fn browser_attach_over_damaged_settings(rig: &Rig) -> (Host<FakeRuntime>, serde_json::Value) {
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let body = json!({"name": "acme", "repositories": [{"url": REPO}], "memory_mib": 4096});
    assert_eq!(
        api.post("/api/workspaces", &body.to_string()).await.status,
        202
    );
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let _editor = editor_stand_in(rig, "acme").await;
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    (host, reply.json())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_with_unreadable_global_settings_opens_no_window_and_says_why() {
    let rig = Rig::named_ids();
    // A user who turned the clipboard prompt off must never get a window that asks: with the
    // settings unreadable no window opens at all.
    let file = damage_global_settings(&rig);
    let (host, body) = browser_attach_over_damaged_settings(&rig).await;
    assert_eq!(body["opened"], false, "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("the global settings cannot be read")
            && message.contains("fix the settings file, or delete it"),
        "{message}"
    );
    assert!(rig.launcher.browser.lock().unwrap().is_empty());
    // The user's fix works: with the file mended the same attach opens the window.
    std::fs::write(
        &file,
        br#"{"schema_version": 2, "workspace_defaults": {"clipboard_read": "deny"}}"#,
    )
    .unwrap();
    let api = api(&host);
    let target = browser_target(&rig, &api, "acme").await;
    assert_eq!(
        target.settings.clipboard_read.value,
        puddle_settings::ClipboardRead::Deny
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_with_an_unreadable_workspace_settings_file_opens_no_window() {
    let rig = Rig::named_ids();
    write_global_settings(
        &rig,
        &json!({"schema_version": 2, "workspace_defaults": {"clipboard_read": "deny"}}),
    );
    // The workspace exists first (its id is drawn at create); its file is damaged while Puddle
    // is closed.
    let first = rig.start().await;
    let mut events = api(&first).events().await;
    create(&api(&first), &mut events, "acme").await;
    first.shutdown().await;
    drop(first);
    let dir = rig
        .dir
        .path()
        .join("data")
        .join("settings")
        .join("workspaces");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("acme.json"), b"{ not json").unwrap();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    api.post("/api/workspaces/acme/start", "").await;
    events.until(ended("acme"), Duration::from_secs(20)).await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let body = reply.json();
    assert_eq!(body["opened"], false, "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.contains("the settings of workspace acme cannot be read"),
        "{message}"
    );
    assert!(rig.launcher.browser.lock().unwrap().is_empty());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_to_microsoft_s_server_gives_the_window_its_connection_token() {
    let rig = Rig::named_ids();
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let target = browser_target(&rig, &api, "acme").await;
    // The server's connection token is the guest's password file: the window carries it as a
    // cookie, and it is read only when the window asks.
    let Some(puddle_services::workspaces::EditorSignIn::ConnectionToken(token)) = target.login
    else {
        panic!("Microsoft's server signs in with its connection token");
    };
    assert_eq!(token.password().await.unwrap(), "0".repeat(64));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_without_a_window_says_the_editor_opens_from_the_desktop_app() {
    let rig = Rig::named_ids();
    rig.windowless.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(
        reply.json(),
        json!({
            "opened": false,
            "message": "the browser editor opens from the desktop app",
            "open_uri": null
        })
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_window_that_cannot_open_says_why_in_the_browser_attach_answer() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    *rig.launcher.fail_with.lock().unwrap() =
        Some("the editor gave no session (answer 200)".into());
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "browser"}).to_string(),
        )
        .await;
    assert_eq!(reply.json()["opened"], false);
    assert_eq!(
        reply.json()["message"],
        "the editor gave no session (answer 200)"
    );
    host.shutdown().await;
}

/// Has the front mint Microsoft's editor key, which stores the workspace's half of it.
async fn mint_key(origin: &str) {
    let answer = window_sends(origin, "POST", puddle_front::microsoft::MINT_PATH).await;
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_workspace_closes_its_editor_windows() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let forgotten_by_create = rig.launcher.forgotten.lock().unwrap().len();
    let _editor = editor_stand_in(&rig, "acme").await;
    browser_target(&rig, &api, "acme").await;
    assert!(rig.launcher.closed.lock().unwrap().is_empty());

    stop_and_wait(&api, &mut events, "acme").await;
    assert_eq!(*rig.launcher.closed.lock().unwrap(), ["acme"]);
    assert_eq!(
        rig.launcher.forgotten.lock().unwrap().len(),
        forgotten_by_create,
        "a stop keeps the windows' data: the workspace comes back"
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_headless_host_stops_and_deletes_with_no_window_to_close() {
    // `puddle serve` has no window: stopping and deleting work as they did.
    let headless = Rig::named_ids();
    headless.windowless.store(true, Ordering::SeqCst);
    let host = headless.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    delete_and_wait(&api, &mut events, "acme").await;
    assert!(!has_problem(&host, "browser-data:acme"));
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn deleting_a_workspace_deletes_the_editor_s_secret_key_and_the_windows_data() {
    let rig = Rig::named_ids();
    let covering = puddle_settings::MICROSOFT_TERMS_VERSION;
    write_global_settings(&rig, &microsoft_chosen(covering, false));
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let _editor = editor_stand_in(&rig, "acme").await;
    let origin = browser_origin(&rig, &api, "acme").await;
    served_at(&origin).await;
    mint_key(&origin).await;
    let key = HostPaths::new(rig.dir.path().join("data"))
        .front_keys()
        .join("acme.key");
    let first = std::fs::read_to_string(&key).expect("the front stored Microsoft's key half");

    stop_and_wait(&api, &mut events, "acme").await;
    assert!(
        key.exists(),
        "a stop keeps the key: the workspace comes back"
    );
    delete_and_wait(&api, &mut events, "acme").await;
    assert!(!key.exists(), "a delete removes the key");
    assert_eq!(*rig.launcher.forgotten.lock().unwrap(), ["acme", "acme"]);
    assert_eq!(
        *rig.launcher.closed.lock().unwrap(),
        ["acme", "acme"],
        "a stop and a delete each close the windows first"
    );
    assert!(!has_problem(&host, "browser-data:acme"));

    // A workspace of the same name gets another id, so another origin and another key; the
    // old one stays gone.
    create_as(&api, &mut events, "acme", "acme-2").await;
    let _editor = editor_stand_in(&rig, "acme-2").await;
    let again = browser_origin(&rig, &api, "acme-2").await;
    assert_ne!(again, origin);
    served_at(&again).await;
    mint_key(&again).await;
    let new_key = key.with_file_name("acme-2.key");
    assert_ne!(std::fs::read_to_string(&new_key).unwrap(), first);
    assert!(!key.exists());
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delete_that_cannot_remove_the_editor_s_data_says_so_and_still_deletes() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    // A folder where the key file would be: removing it as a file fails.
    let key = HostPaths::new(rig.dir.path().join("data"))
        .front_keys()
        .join("acme.key");
    std::fs::create_dir_all(&key).unwrap();
    *rig.launcher.forget_fails_with.lock().unwrap() = Some("a window still holds it".into());

    delete_and_wait(&api, &mut events, "acme").await;
    let problem = problem_of(&host, "browser-data:acme");
    assert_eq!(problem.title, "acme's browser editor data was not deleted");
    assert!(
        problem.detail.contains("secret key could not be deleted")
            && problem.detail.contains("a window still holds it"),
        "both failures are named: {}",
        problem.detail
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_workspace_clears_browser_data_under_its_own_id_and_leaves_an_earlier_ids_alone() {
    // A delete cut short (a crash, `puddle serve` with no window to clean) leaves the webview
    // folder and the key half behind. The id is never drawn again, so the data stays where it
    // is; the create still clears under its own id, a second guard for data a window wrote after
    // the draw. The recording launcher only notes the ask; the key half is a real file.
    let rig = Rig::named_ids();
    let key = HostPaths::new(rig.dir.path().join("data"))
        .front_keys()
        .join("acme.key");
    std::fs::create_dir_all(key.parent().unwrap()).unwrap();
    std::fs::write(&key, b"left by an earlier acme").unwrap();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_as(&api, &mut events, "acme", "acme-2").await;
    assert_eq!(*rig.launcher.forgotten.lock().unwrap(), ["acme-2"]);
    assert_eq!(std::fs::read(&key).unwrap(), b"left by an earlier acme");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_duplicate_create_leaves_the_running_workspaces_browser_data_alone() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let asked = rig.launcher.forgotten.lock().unwrap().len();
    let again = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(again.status, 409, "{}", again.body);
    assert_eq!(rig.launcher.forgotten.lock().unwrap().len(), asked);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_data_a_delete_could_not_remove_stays_under_an_id_no_new_workspace_gets() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    stop_and_wait(&api, &mut events, "acme").await;
    *rig.launcher.forget_fails_with.lock().unwrap() = Some("a window still holds it".into());
    delete_and_wait(&api, &mut events, "acme").await;
    assert!(has_problem(&host, "browser-data:acme"));
    let problem = problem_of(&host, "browser-data:acme");
    assert!(
        problem
            .detail
            .contains("No later workspace gets the id acme"),
        "{problem:?}"
    );
    *rig.launcher.forget_fails_with.lock().unwrap() = None;
    // The next workspace of the name has another id: it asks to clear only its own.
    create_as(&api, &mut events, "acme", "acme-2").await;
    assert_eq!(
        rig.launcher
            .forgotten
            .lock()
            .unwrap()
            .last()
            .map(String::as_str),
        Some("acme-2")
    );
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_create_that_cannot_clear_left_over_browser_data_is_refused() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    *rig.launcher.forget_fails_with.lock().unwrap() = Some("a window still holds it".into());
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert!(
        reply.body.contains("a window still holds it") && reply.body.contains("browser data"),
        "{}",
        reply.body
    );
    let list = api.get("/api/workspaces").await;
    assert!(
        !list.body.contains("acme"),
        "no workspace was made: {}",
        list.body
    );
    // The refused create's id is retired and saved: the next one of the name gets another.
    let book: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(list_path(&rig)).unwrap()).unwrap();
    assert_eq!(book["retired_ids"], json!(["acme"]), "{book}");
    *rig.launcher.forget_fails_with.lock().unwrap() = None;
    let mut events = api.events().await;
    create_as(&api, &mut events, "acme", "acme-2").await;
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_create_still_answers_when_its_retired_id_cannot_be_saved() {
    let rig = Rig::new();
    let host = rig.start().await;
    let api = api(&host);
    *rig.launcher.forget_fails_with.lock().unwrap() = Some("a window still holds it".into());
    make_unwritable(&list_path(&rig));
    let reply = api.post("/api/workspaces", &new_workspace("acme")).await;
    assert_eq!(reply.status, 409, "{}", reply.body);
    assert!(
        reply.body.contains("a window still holds it"),
        "{}",
        reply.body
    );
    host.shutdown().await;
}
