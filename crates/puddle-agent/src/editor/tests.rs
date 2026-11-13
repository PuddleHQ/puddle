// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn server() -> EditorServer {
    EditorServer::new("/workspaces/a/.puddle/code-server", "/workspaces/a", 300)
}

#[test]
fn the_command_line_binds_loopback_keeps_data_on_the_volume_and_never_holds_the_password() {
    let args: Vec<String> = arguments(&server())
        .into_iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let after = |flag: &str| {
        let at = args.iter().position(|a| a == flag).unwrap();
        args[at + 1].as_str()
    };
    assert_eq!(after("--bind-addr"), "127.0.0.1:41337");
    assert_eq!(after("--auth"), "password");
    // Built with `Path::join` like the code: the agent runs in the guest, but its unit tests
    // also run on a Windows host, where the separator differs.
    let under = |base: &str, leaf: &str| Path::new(base).join(leaf).to_string_lossy().into_owned();
    let data = "/workspaces/a/.puddle/code-server";
    assert_eq!(after("--user-data-dir"), under(data, "user-data"));
    assert_eq!(after("--extensions-dir"), under(data, "extensions"));
    assert_eq!(after("--config"), under(data, "config.yaml"));
    assert_eq!(after("--reconnection-grace-time"), "300");
    assert!(args.iter().any(|a| a == "--disable-telemetry"));
    assert!(args.iter().any(|a| a == "--disable-update-check"));
    assert!(args.iter().any(|a| a == "--ignore-last-opened"));
    assert!(args.iter().any(|a| a == "--disable-workspace-trust"));
    assert_eq!(args.last().map(String::as_str), Some("/workspaces/a"));
    assert!(
        !args
            .iter()
            .any(|a| a.contains("password") && a != "password")
    );
}

#[test]
fn the_server_opens_the_multi_root_file_only_while_it_lists_a_folder() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("a.code-workspace");
    let server =
        EditorServer::new("/d", "/workspaces/a", 300).with_workspace_file(file.to_string_lossy());
    let opens = || target(&server).to_string_lossy().into_owned();
    // No file yet, a file of settings alone, a list with no folder in it: the folder.
    assert_eq!(opens(), "/workspaces/a");
    for text in [
        r#"{"settings": {"path": 1}}"#,
        "",
        r#"{"folders": []}"#,
        "not json at all",
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(opens(), "/workspaces/a", "{text:?}");
    }
    // A list with a folder, comments and all: the file, which is the last argument.
    std::fs::write(
        &file,
        "// kept\n{\"folders\": [{\"path\": \"/workspaces/a/api\"}], \"settings\": {}}",
    )
    .unwrap();
    assert_eq!(opens(), file.to_string_lossy());
    assert_eq!(
        arguments(&server)
            .last()
            .map(|a| a.to_string_lossy().into_owned()),
        Some(file.to_string_lossy().into_owned())
    );
    // A server told of no file never looks.
    let plain = EditorServer::new("/d", "/workspaces/a", 300);
    assert_eq!(target(&plain), OsString::from("/workspaces/a"));
}

/// A probe that says "taken" until `free` is set, counting how often it was asked.
fn probe(
    asked: Arc<std::sync::atomic::AtomicUsize>,
    free: Arc<std::sync::atomic::AtomicBool>,
) -> impl FnMut(u16) -> std::future::Ready<bool> {
    use std::sync::atomic::Ordering::SeqCst;
    move |_| {
        asked.fetch_add(1, SeqCst);
        std::future::ready(!free.load(SeqCst))
    }
}

#[tokio::test(start_paused = true)]
async fn a_taken_port_is_asked_about_again_and_again_until_it_is_free() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
    let (asked, free) = (
        Arc::new(AtomicUsize::new(0)),
        Arc::new(AtomicBool::new(false)),
    );
    let waiting = tokio::spawn(wait_until_free(
        41337,
        probe(Arc::clone(&asked), Arc::clone(&free)),
    ));
    // Ten poll intervals with the port taken: still waiting, and it kept asking.
    for _ in 0..10 {
        tokio::time::advance(PORT_POLL).await;
        tokio::task::yield_now().await;
    }
    assert!(
        !waiting.is_finished(),
        "it went on while the port was taken"
    );
    assert!(
        asked.load(SeqCst) >= 10,
        "asked {} times",
        asked.load(SeqCst)
    );
    free.store(true, SeqCst);
    tokio::time::advance(PORT_POLL).await;
    tokio::time::timeout(PORT_POLL, waiting)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn a_port_with_a_listener_is_taken_and_a_free_one_is_not() {
    let port = puddle_test_kit::quiet_port();
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .unwrap();
    assert!(port_taken(port).await);
    drop(listener);
    assert!(!port_taken(port).await);
}

#[cfg(unix)]
#[tokio::test]
async fn a_connect_that_reached_only_itself_is_not_a_taken_port() {
    // The kernel's simultaneous open: bind a port, then connect to that same port with nothing
    // listening. The connect succeeds, to nobody.
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], puddle_test_kit::quiet_port()));
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.set_reuseaddr(true).unwrap();
    socket.bind(addr).unwrap();
    let joined = socket.connect(addr).await.unwrap();
    assert_eq!(joined.local_addr().unwrap(), joined.peer_addr().unwrap());
    assert!(!has_a_peer(&joined).await);
    // A real connection to a listener has one.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let real = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
        .await
        .unwrap();
    assert!(has_a_peer(&real).await);
}

fn guest_with(files: &[&str]) -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    for file in files {
        let path = root.path().join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    root
}

#[test]
fn a_guest_with_musl_and_no_glibc_loader_cannot_run_the_editor() {
    let alpine = guest_with(&["lib/ld-musl-x86_64.so.1", "usr/lib/libc.musl-x86_64.so.1"]);
    assert!(musl_without_glibc(alpine.path()));
    assert_eq!(unsupported_image(alpine.path()), Some(NEEDS_GLIBC));
}

#[test]
fn glibc_images_and_musl_with_a_glibc_loader_name_are_left_alone() {
    let debian = guest_with(&[
        "lib64/ld-linux-x86-64.so.2",
        "lib/x86_64-linux-gnu/libc.so.6",
    ]);
    assert_eq!(unsupported_image(debian.path()), None);
    let gcompat = guest_with(&["lib/ld-musl-x86_64.so.1", "lib/ld-linux-x86-64.so.2"]);
    assert_eq!(unsupported_image(gcompat.path()), None);
    let bare = guest_with(&["etc/hostname"]);
    assert_eq!(unsupported_image(bare.path()), None);
    assert_eq!(unsupported_image(&bare.path().join("missing")), None);
}

#[tokio::test]
async fn on_a_musl_image_the_install_problem_says_why_and_no_server_starts() {
    let root = guest_with(&["lib/ld-musl-aarch64.so.1"]);
    let dir = tempfile::tempdir().unwrap();
    let mut config = EditorConfig {
        server: EditorServer::new("/nonexistent/release", "/nonexistent/work", 120),
        password_file: dir.path().join("run/editor-password"),
        boot_id: dir.path().join("boot_id"),
        root: root.path().to_path_buf(),
    };
    let control = Control::new(&config.server);
    let mut problem = control.install_problem();
    run_controlled(config.clone(), Offline, Arc::clone(&control)).await;
    problem.changed().await.unwrap();
    assert_eq!(problem.borrow().as_deref(), Some(NEEDS_GLIBC));
    assert!(!config.password_file.exists(), "a server was started");
    // The same agent on a glibc image reports nothing of the kind.
    config.root = dir.path().to_path_buf();
    assert_eq!(unsupported_image(&config.root), None);
}

#[test]
fn passwords_are_long_hex_and_different_every_time() {
    let a = new_password().unwrap();
    let b = new_password().unwrap();
    assert_eq!(a.len(), PASSWORD_BYTES * 2);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[test]
fn only_a_password_this_module_wrote_is_reused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pw");
    assert_eq!(existing_password(&path), None);
    let good = new_password().unwrap();
    for (case, bad) in [
        "",
        "short",
        &"g".repeat(64),
        &format!("{good}\n"),
        &format!("{good}00"),
    ]
    .into_iter()
    .enumerate()
    {
        std::fs::write(&path, bad).unwrap();
        assert_eq!(
            existing_password(&path),
            None,
            "a malformed password file (case {case}) was reused"
        );
    }
    std::fs::write(&path, &good).unwrap();
    assert_eq!(existing_password(&path), Some(good));
}

#[test]
fn a_password_is_kept_within_its_boot_and_replaced_in_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let config = EditorConfig {
        server: server(),
        password_file: dir.path().join("run/editor-password"),
        boot_id: dir.path().join("boot_id"),
        root: dir.path().join("root"),
    };
    std::fs::write(&config.boot_id, "boot-one\n").unwrap();
    let first = boot_password(&config).unwrap();
    assert_eq!(
        std::fs::read_to_string(&config.password_file).unwrap(),
        first
    );
    // A restarted server or agent in the same boot: the same password.
    assert_eq!(boot_password(&config).unwrap(), first);
    // The next boot finds the file on the disk and makes a new one.
    std::fs::write(&config.boot_id, "boot-two\n").unwrap();
    let second = boot_password(&config).unwrap();
    assert_ne!(second, first);
    assert_eq!(
        std::fs::read_to_string(&config.password_file).unwrap(),
        second
    );
    assert_eq!(boot_password(&config).unwrap(), second);
    // A password with no stamp (written before the stamp existed) is not kept.
    std::fs::remove_file(boot_stamp(&config.password_file)).unwrap();
    assert_ne!(boot_password(&config).unwrap(), second);
    // A boot that cannot be named keeps nothing.
    std::fs::remove_file(&config.boot_id).unwrap();
    let unnamed = boot_password(&config).unwrap();
    assert_ne!(boot_password(&config).unwrap(), unnamed);
}

#[test]
fn the_password_file_is_owner_only_and_replaced_whole() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("run/editor-password");
    let first = puddle_types::fixture::password("first");
    let second = puddle_types::fixture::password("second");
    write_password(&path, &first).unwrap();
    write_password(&path, &second).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), second);
    assert!(!dir.path().join("run/editor-password.partial").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn the_server_gets_the_password_in_its_environment_and_none_of_the_editor_hand_over_variables() {
    let inherited = [
        "PATH",
        "VSCODE_IPC_HOOK_CLI",
        "PUDDLE_AGENT_LISTEN",
        "PORT",
        "HOME",
    ]
    .map(String::from);
    let password = puddle_types::fixture::password("editor");
    let launch = Launch {
        program: Path::new("/puddle/code-server").join("bin/code-server"),
        args: arguments(&server()),
        password_env: true,
        build: None,
    };
    let cmd = command(&launch, &password, inherited, None);
    let envs: std::collections::BTreeMap<String, Option<String>> = cmd
        .as_std()
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    assert_eq!(envs.get("PASSWORD"), Some(&Some(password)));
    for gone in ["VSCODE_IPC_HOOK_CLI", "PUDDLE_AGENT_LISTEN", "PORT"] {
        assert_eq!(envs.get(gone), Some(&None), "{gone} is removed");
    }
    for kept in ["PATH", "HOME"] {
        assert!(!envs.contains_key(kept), "{kept} is inherited untouched");
    }
    assert_eq!(
        cmd.as_std().get_program(),
        Path::new("/puddle/code-server")
            .join("bin/code-server")
            .as_os_str()
    );
}

#[test]
fn a_server_run_as_the_session_user_gets_its_login_variables() {
    let launch = Launch {
        program: PathBuf::from("/puddle/code-server/bin/code-server"),
        args: Vec::new(),
        password_env: true,
        build: None,
    };
    let agent = Account::find(
        "agent",
        "agent:x:1001:1001::/home/agent:/bin/bash\n",
        "docker:x:998:agent\n",
    )
    .unwrap();
    let password = puddle_types::fixture::password("editor");
    let cmd = command(&launch, &password, ["HOME".to_owned()], Some(&agent));
    let envs: std::collections::BTreeMap<String, Option<String>> = cmd
        .as_std()
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().into_owned(),
                v.map(|v| v.to_string_lossy().into_owned()),
            )
        })
        .collect();
    for (name, value) in [
        ("HOME", "/home/agent"),
        ("USER", "agent"),
        ("LOGNAME", "agent"),
        ("SHELL", "/bin/bash"),
    ] {
        assert_eq!(envs.get(name), Some(&Some(value.to_owned())), "{name}");
    }
    assert_eq!(envs.get("PASSWORD"), Some(&Some(password)));
}

#[cfg(unix)]
#[test]
fn the_session_user_is_given_the_server_s_folders_and_the_password_file() {
    use std::os::unix::fs::MetadataExt as _;
    let dir = tempfile::tempdir().unwrap();
    let me = std::fs::metadata(dir.path()).unwrap();
    let account = Account {
        name: "me".to_owned(),
        uid: me.uid(),
        gid: me.gid(),
        groups: vec![me.gid()],
        home: "/home/me".to_owned(),
        shell: "/bin/sh".to_owned(),
    };
    let data = dir.path().join("code-server");
    let microsoft = EditorServer::microsoft(data.to_str().unwrap(), "/f", 300, false, false);
    let config = |server: EditorServer| EditorConfig {
        server,
        password_file: dir.path().join("editor-password"),
        boot_id: dir.path().join("boot_id"),
        root: dir.path().join("root"),
    };
    let code_server = config(EditorServer::new(data.to_str().unwrap(), "/f", 300));
    // Each path must exist: a missing one is an error, so none is skipped.
    prepare(&code_server.server).unwrap();
    assert_eq!(
        give_to(&account, &code_server).unwrap_err().kind(),
        io::ErrorKind::NotFound,
        "the password file is not written yet"
    );
    write_password(&code_server.password_file, &new_password().unwrap()).unwrap();
    give_to(&account, &code_server).unwrap();
    let microsoft = config(microsoft);
    assert_eq!(
        give_to(&account, &microsoft).unwrap_err().kind(),
        io::ErrorKind::NotFound,
        "Microsoft's data folder is not made yet"
    );
    microsoft::prepare(&microsoft.server).unwrap();
    give_to(&account, &microsoft).unwrap();
}

#[test]
fn a_switch_made_before_the_server_reads_its_build_is_not_a_restart_request() {
    let dir = tempfile::tempdir().unwrap();
    let builds = dir.path().join("servers");
    let (old, new) = ("1".repeat(40), "2".repeat(40));
    for commit in [&old, &new] {
        let program = builds.join(commit).join(microsoft::PROGRAM);
        std::fs::create_dir_all(program.parent().unwrap()).unwrap();
        std::fs::write(&program, "").unwrap();
        std::fs::write(builds.join(commit).join(".puddle-installed"), "").unwrap();
    }
    std::fs::write(builds.join("current"), &old).unwrap();
    let mut server = EditorServer::microsoft("/d", "/f", 120, false, true);
    server.dir = builds.to_string_lossy().into_owned();
    let control = Control::new(&server);
    assert_eq!(control.claim(), Some((builds.join(&old), false)));
    // Staged and switched while no server runs: the next read gets the new build, on trial,
    // and the request that switch made is spent by that read, not left to stop the server
    // that is about to start.
    std::fs::write(builds.join("staged"), &new).unwrap();
    assert_eq!(control.switch().unwrap(), Some(new.clone()));
    assert_eq!(control.claim(), Some((builds.join(&new), true)));
    assert!(control.restart.notified().now_or_never().is_none());
    // A switch after the read is a request the running server answers.
    std::fs::write(builds.join("staged"), &old).unwrap();
    assert_eq!(control.switch().unwrap(), Some(old));
    assert!(control.restart.notified().now_or_never().is_some());
}

#[test]
fn restarts_wait_longer_after_each_quick_exit_up_to_a_limit() {
    let waits: Vec<u64> = [1, 2, 3, 10, 30, 31, 1000]
        .into_iter()
        .map(|n| wait_after(n).as_secs())
        .collect();
    assert_eq!(waits, [1, 2, 3, 10, 30, 30, 30]);
    assert_eq!(wait_after(0).as_secs(), 1);
}

#[test]
fn a_refused_download_waits_a_minute_then_doubles_up_to_an_hour() {
    let waits: Vec<u64> = [0, 1, 2, 3, 6, 7, 8, 1000, u32::MAX]
        .into_iter()
        .map(|n| wait_after_refusal(n).as_secs())
        .collect();
    assert_eq!(waits, [60, 60, 120, 240, 1920, 3600, 3600, 3600, 3600]);
}

/// An update service that answers every metadata request with one build and a published hash
/// that is not the tarball's, and counts the downloads.
struct WrongHash {
    downloads: std::sync::atomic::AtomicUsize,
}

impl Fetch for Arc<WrongHash> {
    async fn text(&self, url: &str) -> io::Result<String> {
        tokio::task::yield_now().await; // a network answer is not instant
        let commit = "e".repeat(40);
        Ok(if url.contains("/api/latest/") {
            format!(r#"{{"version":"{commit}","productVersion":"1.0.0"}}"#)
        } else {
            format!(
                r#"{{"version":"{commit}","sha256hash":"{}"}}"#,
                "0".repeat(64)
            )
        })
    }

    async fn file(&self, _url: &str, dest: &Path) -> io::Result<()> {
        tokio::task::yield_now().await; // a download is not instant
        self.downloads
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(dest, b"not the published file")
    }
}

/// A Microsoft server config on a fresh volume, and the service that refuses every build.
fn refusing(dir: &Path, auto_update: bool) -> (EditorConfig, Arc<WrongHash>) {
    let mut server = EditorServer::microsoft("/d", "/f", 120, false, auto_update);
    server.dir = dir.join("servers").to_string_lossy().into_owned();
    server.data = dir.join("data").to_string_lossy().into_owned();
    let config = EditorConfig {
        server,
        password_file: dir.join("password"),
        boot_id: dir.join("boot_id"),
        root: dir.join("root"),
    };
    let service = Arc::new(WrongHash {
        downloads: std::sync::atomic::AtomicUsize::new(0),
    });
    (config, service)
}

#[tokio::test(start_paused = true)]
async fn a_download_with_the_wrong_hash_is_fetched_on_a_slowing_schedule_not_every_30_seconds() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), false);
    let running = tokio::spawn(run_with(config, Arc::clone(&service)));
    // Downloads at 0, 1, 3, 7, 15, 31 and 63 minutes; the next is at 123. Every 30 seconds
    // would be 200.
    tokio::time::sleep(Duration::from_mins(100)).await;
    assert_eq!(service.downloads.load(SeqCst), 7);
    running.abort();
}

#[tokio::test(start_paused = true)]
async fn every_refused_download_is_told_to_the_host_in_words_without_checksums() {
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), false);
    let control = Control::new(&config.server);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let running = tokio::spawn(run_controlled(config, service, Arc::clone(&control)));
    let wanted = AgentMessage::editor_status(
        EditorProblemKind::Install,
        Some("the download does not match the checksum Microsoft published for it"),
    );
    // The first refusal, then the next one a minute later: a host that missed the first
    // (it restarted) hears the second.
    assert_eq!(host.recv().await, Some(wanted.clone()));
    assert_eq!(host.recv().await, Some(wanted));
    running.abort();
}

#[tokio::test(start_paused = true)]
async fn an_install_that_works_ends_the_report_once_and_says_nothing_when_nothing_was_reported() {
    let control = Control::new(&EditorServer::microsoft("/d", "/f", 120, false, false));
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    control.install_worked();
    puddle_test_kit::no_event_for(
        "a message when nothing was wrong",
        Duration::from_secs(1),
        &mut host,
    )
    .await;
    control.install_failed("nope");
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::Install,
            Some("nope")
        ))
    );
    control.install_worked();
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::Install,
            None
        ))
    );
    control.install_worked();
    puddle_test_kit::no_event_for("a second end", Duration::from_secs(1), &mut host).await;
}

#[tokio::test(start_paused = true)]
async fn a_server_that_runs_as_root_for_want_of_its_user_is_told_each_start_and_ends_when_it_is_found()
 {
    let agent = Account::find("agent", "agent:x:1001:1001::/home/agent:/bin/sh\n", "").unwrap();
    let named = EditorServer::new("/d", "/f", 300).with_user(puddle_types::SessionUser::Agent);
    let control = Control::new(&named);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    // Root by choice, and a user that is there, say nothing.
    assert_eq!(
        account(
            &EditorServer::new("/d", "/f", 300),
            &control,
            |_| unreachable!()
        ),
        None
    );
    assert_eq!(
        account(&named, &control, |name| {
            assert_eq!(name, "agent");
            Ok(agent.clone())
        }),
        Some(agent.clone())
    );
    puddle_test_kit::no_event_for("a user that is there", Duration::from_secs(1), &mut host).await;
    // A user the guest lacks: the server runs as root, and the host hears why at each start.
    let lacking = |_: &str| Err(io::Error::new(io::ErrorKind::NotFound, "no entry"));
    let wanted = AgentMessage::editor_status(
        EditorProblemKind::SessionUser,
        Some(
            "the session user agent is missing in the guest (no entry), so the editor server runs as root",
        ),
    );
    assert_eq!(account(&named, &control, lacking), None);
    assert_eq!(host.recv().await, Some(wanted.clone()));
    assert_eq!(account(&named, &control, lacking), None);
    assert_eq!(host.recv().await, Some(wanted));
    // A start that finds the user ends it, once.
    assert_eq!(
        account(&named, &control, |_| Ok(agent.clone())),
        Some(agent.clone())
    );
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::SessionUser,
            None
        ))
    );
    assert_eq!(
        account(&named, &control, |_| Ok(agent.clone())),
        Some(agent)
    );
    puddle_test_kit::no_event_for("a second end", Duration::from_secs(1), &mut host).await;
    // Root by choice after a lacking start ends it too.
    assert_eq!(account(&named, &control, lacking), None);
    assert!(host.recv().await.is_some());
    assert_eq!(
        account(
            &EditorServer::new("/d", "/f", 300),
            &control,
            |_| unreachable!()
        ),
        None
    );
    assert_eq!(
        host.recv().await,
        Some(AgentMessage::editor_status(
            EditorProblemKind::SessionUser,
            None
        ))
    );
}

#[tokio::test(start_paused = true)]
async fn a_restart_request_ends_the_wait_after_a_refused_download() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), false);
    let control = Control::new(&config.server);
    let running = tokio::spawn(run_controlled(
        config,
        Arc::clone(&service),
        Arc::clone(&control),
    ));
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(service.downloads.load(SeqCst), 1);
    control.restart.notify_one();
    tokio::time::sleep(Duration::from_secs(10)).await;
    assert_eq!(service.downloads.load(SeqCst), 2, "tried again at once");
    running.abort();
}

#[tokio::test(start_paused = true)]
async fn a_refused_update_is_looked_for_on_a_slowing_schedule_too() {
    use std::sync::atomic::Ordering::SeqCst;
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), true);
    let times = UpdateTimes {
        first: Duration::from_secs(1),
        every: Duration::from_hours(24),
        retry: Duration::from_mins(1),
    };
    let control = Control::with_times(&config.server, times);
    let looking = tokio::spawn(update_loop(control, Arc::new(Arc::clone(&service))));
    // Looks at 1 s and then after 1, 2, 4, 8, 16 and 32 minutes (7 in all); every minute would be 100.
    tokio::time::sleep(Duration::from_mins(100)).await;
    assert_eq!(service.downloads.load(SeqCst), 7);
    looking.abort();
}

/// An update service that cannot be reached.
struct Offline;

impl Fetch for Offline {
    async fn text(&self, _url: &str) -> io::Result<String> {
        tokio::task::yield_now().await; // a network answer is not instant
        Err(io::Error::other("no route to host"))
    }

    async fn file(&self, _url: &str, _dest: &Path) -> io::Result<()> {
        tokio::task::yield_now().await;
        Err(io::Error::other("no route to host"))
    }
}

fn times(retry: Duration) -> UpdateTimes {
    UpdateTimes {
        first: Duration::from_secs(1),
        every: Duration::from_hours(24),
        retry,
    }
}

fn update_message(reason: Option<&str>) -> AgentMessage {
    AgentMessage::editor_status(EditorProblemKind::Update, reason)
}

#[tokio::test(start_paused = true)]
async fn a_refused_newer_build_is_told_as_an_update_problem_until_a_look_works() {
    let dir = tempfile::tempdir().unwrap();
    let (config, service) = refusing(dir.path(), true);
    let control = Control::with_times(&config.server, times(Duration::from_mins(1)));
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let looking = tokio::spawn(update_loop(control, Arc::new(Arc::clone(&service))));
    assert_eq!(
        host.recv().await,
        Some(update_message(Some(
            "a newer build was refused: the download does not match the checksum Microsoft published for it"
        )))
    );
    // The build is now known to be bad (as after a failed start), so the next look is quiet.
    std::fs::write(dir.path().join("servers/rejected"), "e".repeat(40)).unwrap();
    assert_eq!(host.recv().await, Some(update_message(None)));
    looking.abort();
}

#[tokio::test(start_paused = true)]
async fn looks_that_fail_are_told_only_after_five_in_a_row() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), true);
    let control = Control::with_times(&config.server, times(Duration::from_mins(1)));
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let looking = tokio::spawn(update_loop(control, Arc::new(Offline)));
    // Looks at 1 s, then every minute: four have failed at 200 s.
    puddle_test_kit::no_event_for(
        "a report before the fifth failed look",
        Duration::from_secs(200),
        &mut host,
    )
    .await;
    let Some(AgentMessage::EditorStatus {
        kind: EditorProblemKind::Update,
        problem: Some(reason),
    }) = host.recv().await
    else {
        panic!("no update problem after the fifth failed look");
    };
    assert!(
        reason.starts_with("looking for a newer build keeps failing")
            && reason.contains("no route to host"),
        "{reason}"
    );
    looking.abort();
}

#[tokio::test(start_paused = true)]
async fn a_first_install_that_keeps_failing_on_the_network_is_told_after_five_tries() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), false);
    let control = Control::new(&config.server);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    let running = tokio::spawn(run_controlled(config, Offline, Arc::clone(&control)));
    // Tries at 0, 1, 3, 6 and 16 s: four have failed at 10 s.
    puddle_test_kit::no_event_for(
        "a report before the fifth failed try",
        Duration::from_secs(10),
        &mut host,
    )
    .await;
    let Some(AgentMessage::EditorStatus {
        kind: EditorProblemKind::Install,
        problem: Some(reason),
    }) = host.recv().await
    else {
        panic!("no install problem after the fifth failed try");
    };
    assert!(
        reason.starts_with("the download keeps failing") && reason.contains("no route to host"),
        "{reason}"
    );
    running.abort();
}

#[tokio::test(start_paused = true)]
async fn a_build_rolled_back_after_a_bad_start_is_told_and_one_that_stays_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let (config, _) = refusing(dir.path(), true);
    let builds = Path::new(&config.server.dir);
    let (old, new) = ("a".repeat(40), "b".repeat(40));
    for commit in [&old, &new] {
        let build = builds.join(commit);
        std::fs::create_dir_all(build.join("bin")).unwrap();
        std::fs::write(build.join(".puddle-installed"), b"").unwrap();
        std::fs::write(build.join(microsoft::PROGRAM), b"").unwrap();
    }
    std::fs::write(builds.join("current"), &new).unwrap();
    std::fs::write(builds.join("previous"), &old).unwrap();
    let control = Control::new(&config.server);
    let (out, mut host) = mpsc::channel(8);
    tokio::spawn(forward_editor_problems(
        control.install_problem(),
        control.update_problem(),
        control.session_user_problem(),
        out,
    ));
    // A build that is not the one on trial is left alone and nothing is said.
    settle_trial(&control, &old).await;
    puddle_test_kit::no_event_for(
        "a report for no rollback",
        Duration::from_secs(1),
        &mut host,
    )
    .await;
    settle_trial(&control, &new).await;
    assert_eq!(
        host.recv().await,
        Some(update_message(Some(
            "the newer build exited before it started, so the previous one runs again"
        )))
    );
}
