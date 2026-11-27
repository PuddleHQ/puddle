// SPDX-License-Identifier: AGPL-3.0-or-later
use super::update_tests::Offline;
use super::*;

pub(super) fn server() -> EditorServer {
    EditorServer::new("/workspace/.puddle/code-server", "/workspace", 300)
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
    let data = "/workspace/.puddle/code-server";
    assert_eq!(after("--user-data-dir"), under(data, "user-data"));
    assert_eq!(after("--extensions-dir"), under(data, "extensions"));
    assert_eq!(after("--config"), under(data, "config.yaml"));
    assert_eq!(after("--reconnection-grace-time"), "300");
    assert!(args.iter().any(|a| a == "--disable-telemetry"));
    assert!(args.iter().any(|a| a == "--disable-update-check"));
    assert!(args.iter().any(|a| a == "--ignore-last-opened"));
    assert!(args.iter().any(|a| a == "--disable-workspace-trust"));
    assert_eq!(args.last().map(String::as_str), Some("/workspace"));
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
        EditorServer::new("/d", "/workspace", 300).with_workspace_file(file.to_string_lossy());
    let opens = || target(&server).to_string_lossy().into_owned();
    // No file yet, a file of settings alone, a list with no folder in it: the folder.
    assert_eq!(opens(), "/workspace");
    for text in [
        r#"{"settings": {"path": 1}}"#,
        "",
        r#"{"folders": []}"#,
        "not json at all",
    ] {
        std::fs::write(&file, text).unwrap();
        assert_eq!(opens(), "/workspace", "{text:?}");
    }
    // A list with a folder, comments and all: the file, which is the last argument.
    std::fs::write(
        &file,
        "// kept\n{\"folders\": [{\"path\": \"/workspace/api\"}], \"settings\": {}}",
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
    let plain = EditorServer::new("/d", "/workspace", 300);
    assert_eq!(target(&plain), OsString::from("/workspace"));
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
    let cmd = command(&launch, Some(&password), inherited, None);
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
    let cmd = command(&launch, Some(&password), ["HOME".to_owned()], Some(&agent));
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
    use puddle_agent_proto::editor::MicrosoftOptions;
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
    let microsoft = EditorServer::microsoft(
        data.to_str().unwrap(),
        "/f",
        300,
        MicrosoftOptions {
            telemetry: false,
            auto_update: false,
        },
    );
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
fn the_first_open_follows_the_colour_scheme_and_a_later_start_keeps_the_users_settings() {
    let dir = tempfile::tempdir().unwrap();
    let server = EditorServer::new(dir.path().to_str().unwrap(), "/f", 300);
    prepare(&server).unwrap();
    let file = dir.path().join("user-data/User/settings.json");

    let made = seed_first_settings(&server).unwrap();
    assert_eq!(
        made,
        vec![file.parent().unwrap().to_path_buf(), file.clone()]
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains(r#""window.autoDetectColorScheme": true"#),
        "{text}"
    );
    assert!(text.contains("Default Dark Modern") && text.contains("Default Light Modern"));
    assert!(
        text.contains(r#""telemetry.telemetryLevel": "off""#),
        "{text}"
    );
    assert!(
        text.contains(r#""workbench.enableExperiments": false"#),
        "{text}"
    );

    std::fs::write(&file, r#"{"workbench.colorTheme": "Solarized Dark"}"#).unwrap();
    assert_eq!(seed_first_settings(&server).unwrap(), Vec::<PathBuf>::new());
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        r#"{"workbench.colorTheme": "Solarized Dark"}"#,
        "a later start leaves the user's file alone"
    );
}
