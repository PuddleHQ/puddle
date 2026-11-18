// SPDX-License-Identifier: AGPL-3.0-or-later
//! What an attach opens, launcher failures and pull audits.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_editor_opens_the_start_folder_or_the_multi_root_file_on_the_workspaces_ssh_host()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    let attach = || async {
        let reply = api
            .post(
                "/api/workspaces/acme/attach",
                &json!({"mode": "desktop"}).to_string(),
            )
            .await;
        assert_eq!(reply.json()["opened"], true, "{}", reply.body);
        rig.launcher
            .targets
            .lock()
            .unwrap()
            .last()
            .cloned()
            .unwrap()
    };
    let root = attach().await;
    assert_eq!(root.ssh_host, "puddle-acme");
    assert_eq!(root.path, "/workspace");
    assert!(!root.is_workspace_file);

    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    rig.guest
        .put_checkout("web", "https://example.org/acme/web.git");
    let multi = attach().await;
    assert_eq!(multi.path, "/workspace/.puddle/editor/acme.code-workspace");
    assert!(multi.is_workspace_file);

    let reply = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let folder = attach().await;
    assert_eq!(folder.path, "/workspace/api");
    assert!(!folder.is_workspace_file);
    host.shutdown().await;
}

/// An image with no git can't describe checkouts; the editor still opens the volume root and the
/// attach raises no problem about it.
#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_attach_on_an_image_without_git_opens_the_folder_and_raises_no_problem() {
    let rig = Rig::named_ids();
    rig.guest.git_missing.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(reply.json()["opened"], true, "{}", reply.body);
    let target = rig
        .launcher
        .targets
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(target.path, "/workspace");
    assert!(!target.is_workspace_file);
    assert!(problem_is(&api, "editor-file:acme", false).await.is_none());
    host.shutdown().await;
}

/// On an image with no git, two checkouts still open the multi-root window: the editor's listing
/// falls back to the checkout names, which need no git.
#[tokio::test(flavor = "multi_thread")]
async fn a_desktop_attach_on_an_image_without_git_with_two_checkouts_opens_the_multi_root_window() {
    let rig = Rig::named_ids();
    rig.guest.git_missing.store(true, Ordering::SeqCst);
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    rig.guest
        .put_checkout("web", "https://example.org/acme/web.git");
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(reply.json()["opened"], true, "{}", reply.body);
    let target = rig
        .launcher
        .targets
        .lock()
        .unwrap()
        .last()
        .cloned()
        .unwrap();
    assert_eq!(target.path, "/workspace/.puddle/editor/acme.code-workspace");
    assert!(target.is_workspace_file);
    assert!(problem_is(&api, "editor-file:acme", false).await.is_none());
    // The checkouts screen still says why it has nothing to show.
    let list = checkouts_of(&api, "acme").await;
    assert!(list["problem"].as_str().unwrap().contains("git"), "{list}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_browser_editor_gets_the_same_place_to_open_as_the_desktop_one_with_the_server_already_running()
 {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create(&api, &mut events, "acme").await;
    let open = || async { browser_target(&rig, &api, "acme").await.open.unwrap() };
    let root = open().await;
    assert_eq!(root.path, "/workspace");
    assert!(!root.is_workspace_file);

    // The workspace keeps running; two checkouts appear, and the attach alone brings the file.
    rig.guest
        .put_checkout("api", "https://example.org/acme/api.git");
    rig.guest
        .put_checkout("web", "https://example.org/acme/web.git");
    let multi = open().await;
    assert_eq!(multi.path, "/workspace/.puddle/editor/acme.code-workspace");
    assert!(multi.is_workspace_file);

    let reply = put_start_folder(&api, "acme", Some("api")).await;
    assert_eq!(reply.status, 200, "{}", reply.body);
    let folder = open().await;
    assert_eq!(folder.path, "/workspace/api");
    assert!(!folder.is_workspace_file);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_launcher_that_fails_says_why_in_the_answer() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    *rig.launcher.fail_with.lock().unwrap() = Some("VS Code is not installed".into());
    let reply = api
        .post(
            "/api/workspaces/acme/attach",
            &json!({"mode": "desktop"}).to_string(),
        )
        .await;
    assert_eq!(reply.json()["opened"], false, "{}", reply.body);
    assert_eq!(reply.json()["message"], "VS Code is not installed");
    host.shutdown().await;
}

/// An endpoint that cannot open (its folder is gone) is reported as an event and a problem, the
/// attach says so, and the problem ends when the endpoint opens again.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn an_endpoint_that_cannot_open_is_reported_and_ends_when_it_can() {
    let rig = Rig::named_ids();
    let host = rig.start().await;
    let api = api(&host);
    let mut events = api.events().await;
    let name = WorkspaceId::new("acme").unwrap();
    create_with_direct_ssh(&api, &mut events, "acme", true).await;
    let endpoint = host.workspaces().ssh_endpoint(&name).await.unwrap();
    let folder = endpoint.parent().unwrap().to_path_buf();
    allow_direct_ssh(&api, "acme", false).await;
    let moved = folder.with_extension("away");
    std::fs::rename(&folder, &moved).unwrap();

    allow_direct_ssh(&api, "acme", true).await;
    let (title, detail) = problem_is(&api, "ssh:acme", true).await.unwrap();
    assert!(title.contains("acme"), "{title}");
    assert!(detail.contains("no SSH endpoint"), "{detail}");
    let event = events
        .until(
            |e| e["type"] == "workspace_ssh_failed",
            Duration::from_secs(20),
        )
        .await;
    assert_eq!(event["workspace"], "acme");
    assert!(host.workspaces().ssh_endpoint(&name).await.is_none());
    assert!(ssh_hosts(&rig).is_empty());
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
            .contains("no endpoint")
    );

    // The folder is back: turning the switch off and on again opens it and ends the problem.
    std::fs::rename(&moved, &folder).unwrap();
    allow_direct_ssh(&api, "acme", false).await;
    assert!(problem_is(&api, "ssh:acme", false).await.is_none());
    allow_direct_ssh(&api, "acme", true).await;
    host.workspaces().ssh_endpoint(&name).await.unwrap();
    assert_eq!(ssh_hosts(&rig), ["puddle-acme"]);
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_image_pull_is_audited_as_puddles_own_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rig = Rig::new();
    let platform = FakePlatform::new(&rig.log);
    let prepared = prepare(rig.config(), &platform).unwrap();
    let factory = FakeFactory::new(&rig.runtime, &rig.log);
    let host = Host::start(prepared, &factory, hermetic_options())
        .await
        .unwrap();

    let target = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let target_port = target.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = target.accept().await;
    });
    let url = factory.pull_url.lock().unwrap().clone().unwrap();
    let rest = url.strip_prefix("http://").unwrap();
    let (credentials, addr) = rest.split_once('@').unwrap();
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!(
        "CONNECT 127.0.0.1:{target_port} HTTP/1.1\r\nHost: 127.0.0.1:{target_port}\r\nProxy-Authorization: Basic {}\r\n\r\n",
        base64(credentials.as_bytes())
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut head = [0_u8; 12];
    stream.read_exact(&mut head).await.unwrap();
    assert!(head.starts_with(b"HTTP/1.1 200"), "{head:?}");
    drop(stream);

    let record = eventually(
        "an audit record for the pull",
        Duration::from_secs(5),
        || {
            host.store()
                .audit_lines(0, 1000)
                .unwrap()
                .into_iter()
                .filter_map(|(_, line)| serde_json::from_str(&line).ok())
                .find(|v: &serde_json::Value| v["type"] == "connection")
        },
    )
    .await;
    assert!(record["workspace"].is_null(), "{record}");
    assert_eq!(record["origin"], "puddle", "{record}");
    host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stopped_host_has_closed_its_database_so_a_restart_never_waits_on_it() {
    // SQLite deletes the write-ahead log when the last connection closes; a close still
    // running (a checkpoint that fsyncs) is what made a quick restart hit "database is locked"
    // when the disk stalled.
    let rig = Rig::new();
    let wal = rig.dir.path().join("data").join("puddle.db-wal");
    for _ in 0..20 {
        let host = rig.start().await;
        host.shutdown().await;
        drop(host);
        assert!(
            !wal.exists(),
            "the database was still open after the host was dropped; handles on it: {:?}",
            handles_on_the_database(rig.dir.path())
        );
    }
}

/// The files of `dir` this process still has open (`/proc/self/fd`), so a log left behind names
/// what held the database. Nothing is listed where the OS has no such view.
fn handles_on_the_database(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(fds) = std::fs::read_dir("/proc/self/fd") else {
        return Vec::new();
    };
    fds.filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
        .filter(|target| target.starts_with(dir))
        .collect()
}
