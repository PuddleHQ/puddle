// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_api::OpenTarget;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use bytes::Bytes;
use http::header::SET_COOKIE;
use http::{Request, Response};
use http_body_util::{BodyExt, Full};
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use puddle_api::EditorLogin;
use puddle_types::WorkspaceId;
use puddle_types::fixture::password;
use tauri::Manager;
use tokio::net::TcpListener;

use super::*;
use crate::sandbox_window::Hooks;

/// A front that signs in anyone with `password` and counts the logins it answered.
async fn front(password: &str) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let logins = Arc::new(Mutex::new(Vec::new()));
    let seen = logins.clone();
    let password = password.to_owned();
    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = seen.clone();
            let password = password.clone();
            let service = service_fn(move |request: Request<hyper::body::Incoming>| {
                let seen = seen.clone();
                let password = password.clone();
                async move {
                    let body = request.into_body().collect().await.unwrap().to_bytes();
                    let body = String::from_utf8(body.to_vec()).unwrap();
                    seen.lock().unwrap().push(body.clone());
                    let mut answer = Response::builder().status(200);
                    if body == format!("password={password}") {
                        answer = answer
                            .status(302)
                            .header(SET_COOKIE, "code-server-session=abc123; Path=/; HttpOnly");
                    }
                    Ok::<_, std::convert::Infallible>(
                        answer.body(Full::new(Bytes::from_static(b"page"))).unwrap(),
                    )
                }
            });
            tokio::spawn(
                hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service),
            );
        }
    });
    (addr, logins)
}

struct Password(String);

impl EditorLogin for Password {
    fn password(&self) -> BoxFuture<'_, Result<String, LaunchError>> {
        Box::pin(async move { Ok(self.0.clone()) })
    }
}

fn record(name: &str) -> WorkspaceRecord {
    WorkspaceRecord::new(
        WorkspaceId::new(name).unwrap(),
        puddle_api::DisplayName::parse(name).unwrap(),
        Vec::new(),
    )
}

fn windows() -> SandboxWindows {
    SandboxWindows::new(Hooks {
        open_external: Arc::new(|_| {}),
        refused: Arc::new(|_| {}),
        new_window: Arc::new(|_, _| {}),
        last_closed: Arc::new(|_| {}),
    })
}

fn target(front: SocketAddr, login: Option<EditorSignIn>) -> BrowserTarget {
    BrowserTarget {
        origin: format!("http://demo.localhost:{}", front.port()),
        front,
        settings: puddle_settings::resolve(
            &puddle_settings::GlobalSettings::default(),
            None::<&puddle_settings::WorkspaceSettings>,
        ),
        login,
        open: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_signs_in_and_opens_the_workspace_s_window_on_the_origin() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
    launcher.install(windows());
    let secret = password("right");
    let (addr, logins) = front(&secret).await;
    let target = target(
        addr,
        Some(EditorSignIn::Form(Arc::new(Password(secret.clone())))),
    );

    launcher
        .open_browser(&record("demo"), &target)
        .await
        .unwrap();

    let window = app.get_webview_window("vs-demo").unwrap();
    assert_eq!(
        window.url().unwrap().as_str(),
        format!("{}/", target.origin)
    );
    assert!(data.path().join("webview").join("demo").is_dir());
    // The password reached the front once, in the login's body, and the address has none of it.
    assert_eq!(*logins.lock().unwrap(), [format!("password={secret}")]);
    assert!(!window.url().unwrap().as_str().contains(&secret));

    // Again: the same window comes to the front, and the editor is not asked to sign in twice.
    launcher
        .open_browser(&record("demo"), &target)
        .await
        .unwrap();
    assert_eq!(app.webview_windows().len(), 1);
    assert_eq!(logins.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_browser_attach_opens_the_multi_root_file_or_the_start_folder_the_target_names() {
    let cases = [
        (
            OpenTarget {
                path: "/workspaces/w/.puddle/editor/w.code-workspace".into(),
                is_workspace_file: true,
            },
            "/?workspace=%2Fworkspaces%2Fw%2F.puddle%2Feditor%2Fw.code-workspace",
        ),
        (
            OpenTarget {
                path: "/workspaces/w/api".into(),
                is_workspace_file: false,
            },
            "/?folder=%2Fworkspaces%2Fw%2Fapi",
        ),
    ];
    for (open, expected) in cases {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
        launcher.install(windows());
        let mut target = target("127.0.0.1:1".parse().unwrap(), None);
        target.open = Some(open);

        launcher
            .open_browser(&record("demo"), &target)
            .await
            .unwrap();

        let window = app.get_webview_window("vs-demo").unwrap();
        assert_eq!(
            window.url().unwrap().as_str(),
            format!("{}{expected}", target.origin)
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_in_browser_attach_navigates_to_the_place_after_the_cookie() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
    launcher.install(windows());
    let secret = password("right");
    let (addr, _logins) = front(&secret).await;
    let mut target = target(addr, Some(EditorSignIn::Form(Arc::new(Password(secret)))));
    target.open = Some(OpenTarget {
        path: "/workspaces/w/.puddle/editor/w.code-workspace".into(),
        is_workspace_file: true,
    });

    launcher
        .open_browser(&record("demo"), &target)
        .await
        .unwrap();

    let url = app
        .get_webview_window("vs-demo")
        .unwrap()
        .url()
        .unwrap()
        .to_string();
    assert!(
        url.starts_with(&format!("{}/?workspace=", target.origin)),
        "{url}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn forgetting_waits_for_the_closing_window_then_removes_its_data_and_session() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
    let windows = windows();
    launcher.install(windows.clone());
    let secret = password("right");
    let (addr, logins) = front(&secret).await;
    let with_login = target(
        addr,
        Some(EditorSignIn::Form(Arc::new(Password(secret.clone())))),
    );
    launcher
        .open_browser(&record("demo"), &with_login)
        .await
        .unwrap();
    let name = WorkspaceId::new("demo").unwrap();
    let folder = data.path().join("webview").join("demo");
    assert!(folder.is_dir());

    // The window ends a moment after it was asked to close: the data goes after that, not before.
    let ending = windows.on_event(name.clone());
    let closing = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        ending(&tauri::WindowEvent::Destroyed);
    });
    let started = tokio::time::Instant::now();
    launcher.forget_browser(&name).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(100));
    closing.await.unwrap();
    assert_eq!(windows.open_count(&name), 0);
    assert!(!folder.exists());

    // The session went too: a workspace of the same name signs in again (the mock keeps its window).
    launcher
        .open_browser(&record("demo"), &with_login)
        .await
        .unwrap();
    assert_eq!(logins.lock().unwrap().len(), 2);
}

#[test]
fn browser_data_is_there_while_either_data_folder_is() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let root = data.path().join("webview");
    let launcher = AppLauncher::new(app.handle().clone(), root.clone());
    let name = WorkspaceId::new("demo").unwrap();
    assert!(!launcher.has_browser_data(&name));
    for folder in ["demo.forward", "demo"] {
        std::fs::create_dir_all(root.join(folder)).unwrap();
        assert!(launcher.has_browser_data(&name), "{folder}");
        std::fs::remove_dir_all(root.join(folder)).unwrap();
    }
    assert!(!launcher.has_browser_data(&name));
    // Another workspace's folder is not this one's.
    std::fs::create_dir_all(root.join("demo2")).unwrap();
    assert!(!launcher.has_browser_data(&name));
}

#[tokio::test(flavor = "multi_thread")]
async fn forgetting_waits_for_a_window_on_a_forwarded_port_that_closes_late() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
    let windows = windows();
    launcher.install(windows.clone());
    let (addr, _) = front(&password("x")).await;
    launcher
        .open_browser(&record("demo"), &target(addr, None))
        .await
        .unwrap();
    let name = WorkspaceId::new("demo").unwrap();
    // The editor's link to a guest port opened a window of its own on the forward.
    windows.forward_opened(&name);
    std::fs::create_dir_all(data.path().join("webview").join("demo.forward")).unwrap();
    assert_eq!(windows.forward_open_count(&name), 1);
    let folder = data.path().join("webview").join("demo.forward");
    assert!(folder.is_dir());

    // The editor window ends at once, the forward window well after the delete's own retries
    // (half a second) would have given up. Nothing may be deleted before it ends.
    windows.on_event(name.clone())(&tauri::WindowEvent::Destroyed);
    let ending = windows.on_forward_event(name.clone());
    let ended = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = ended.clone();
    let closing = tokio::spawn(async move {
        // Simulated latency: a webview that lets go of its window long after the close was asked.
        tokio::time::sleep(Duration::from_millis(900)).await;
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        ending(&tauri::WindowEvent::Destroyed);
    });
    launcher.forget_browser(&name).await.unwrap();
    assert!(
        ended.load(std::sync::atomic::Ordering::SeqCst),
        "the data was deleted while the forward window was open"
    );
    closing.await.unwrap();
    assert_eq!(windows.forward_open_count(&name), 0);
    assert!(!folder.exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn forgetting_does_not_wait_for_ever_for_a_window_that_stays() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"))
        .with_close_wait(Duration::from_millis(50));
    launcher.install(windows());
    let (addr, _) = front(&password("x")).await;
    launcher
        .open_browser(&record("demo"), &target(addr, None))
        .await
        .unwrap();
    launcher
        .forget_browser(&WorkspaceId::new("demo").unwrap())
        .await
        .unwrap();
    assert!(!data.path().join("webview").join("demo").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn forgetting_without_windows_removes_the_data_an_earlier_run_left() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let root = data.path().join("webview");
    let launcher = AppLauncher::new(app.handle().clone(), root.clone());
    let name = WorkspaceId::new("demo").unwrap();
    std::fs::create_dir_all(root.join("demo")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    // Not installed yet, nothing to close; nothing there is also fine.
    launcher.close_browser(&name).await;
    launcher.forget_browser(&name).await.unwrap();
    launcher.forget_browser(&name).await.unwrap();
    assert!(!root.join("demo").exists());
    assert!(
        root.join("other").is_dir(),
        "another workspace's data stays"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn data_that_cannot_be_removed_is_said_in_words() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let root = data.path().join("webview");
    std::fs::create_dir_all(&root).unwrap();
    // A file where the workspace's folder belongs: it cannot be removed as a folder.
    std::fs::write(root.join("demo"), b"x").unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), root);
    launcher.install(windows());
    launcher
        .close_browser(&WorkspaceId::new("demo").unwrap())
        .await;
    let err = launcher
        .forget_browser(&WorkspaceId::new("demo").unwrap())
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("the editor window's browser data could not be removed"),
        "{err}"
    );
}

/// A reader that fails, to show when a token is read.
struct Unreadable;

impl EditorLogin for Unreadable {
    fn password(&self) -> BoxFuture<'_, Result<String, LaunchError>> {
        Box::pin(async { Err(LaunchError::new("demo is not running; start it first")) })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn microsoft_s_server_opens_with_its_connection_token_and_no_login() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
    launcher.install(windows());
    let (addr, logins) = front(&password("unused")).await;
    let token = Some(EditorSignIn::ConnectionToken(Arc::new(Password(password(
        "token",
    )))));
    launcher
        .open_browser(&record("demo"), &target(addr, token))
        .await
        .unwrap();
    let window = app.get_webview_window("vs-demo").unwrap();
    assert!(!window.url().unwrap().as_str().contains(&password("token")));
    // The token is the window's cookie: the login form is never posted.
    assert!(logins.lock().unwrap().is_empty());

    // A token that cannot be read opens no window, and says why.
    let other = tauri::test::mock_app();
    let launcher = AppLauncher::new(other.handle().clone(), data.path().join("webview"));
    launcher.install(windows());
    let err = launcher
        .open_browser(
            &record("demo"),
            &target(
                addr,
                Some(EditorSignIn::ConnectionToken(Arc::new(Unreadable))),
            ),
        )
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "demo is not running; start it first");
    assert!(other.get_webview_window("vs-demo").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_password_opens_no_window_and_says_why() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
    launcher.install(windows());
    let (addr, _) = front(&password("right")).await;
    let err = launcher
        .open_browser(
            &record("demo"),
            &target(
                addr,
                Some(EditorSignIn::Form(Arc::new(Password(password("wrong"))))),
            ),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("gave no session"), "{err}");
    assert!(app.get_webview_window("vs-demo").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_address_that_is_not_a_url_and_a_data_folder_that_cannot_be_made_say_so() {
    let app = tauri::test::mock_app();
    let data = tempfile::tempdir().unwrap();
    // A file where the webview folder belongs: no window can have a data folder in it.
    let blocked = data.path().join("webview");
    std::fs::write(&blocked, b"not a folder").unwrap();
    let launcher = AppLauncher::new(app.handle().clone(), blocked);
    launcher.install(windows());
    let (addr, _) = front(&password("x")).await;
    let mut bad = target(addr, None);
    bad.origin = "not a url".into();
    let err = launcher
        .open_browser(&record("demo"), &bad)
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("the editor's address is not a URL"),
        "{err}"
    );
    let err = launcher
        .open_browser(&record("demo"), &target(addr, None))
        .await
        .unwrap_err();
    assert!(
        err.to_string()
            .starts_with("the editor window did not open"),
        "{err}"
    );
    assert!(app.webview_windows().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn before_the_windows_are_installed_nothing_opens_and_the_user_is_told() {
    let app = tauri::test::mock_app();
    let launcher = AppLauncher::new(app.handle().clone(), "unused".into());
    let (addr, _) = front(&password("x")).await;
    let err = launcher
        .open_browser(&record("demo"), &target(addr, None))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "the editor window is not ready yet; try again in a moment"
    );
    assert!(app.webview_windows().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_desktop_editor_goes_through_the_code_command_and_opens_no_window() {
    let app = tauri::test::mock_app();
    let mut launcher = AppLauncher::new(app.handle().clone(), "unused".into());
    launcher.desktop = VsCodeLauncher::with_program("puddle-test-no-such-code-command");
    launcher.install(windows());
    let target = DesktopTarget {
        ssh_host: "puddle-demo".into(),
        path: "/workspace".into(),
        is_workspace_file: false,
    };
    let err = launcher
        .open_desktop(&record("demo"), &target)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("was not found"), "{err}");
    assert!(app.webview_windows().is_empty());
}

#[test]
fn the_debug_form_names_the_launcher_and_nothing_inside_it() {
    let app = tauri::test::mock_app();
    let launcher = AppLauncher::new(app.handle().clone(), "unused".into());
    assert_eq!(format!("{launcher:?}"), "AppLauncher { .. }");
}
