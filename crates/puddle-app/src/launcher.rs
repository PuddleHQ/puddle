// SPDX-License-Identifier: GPL-3.0-or-later
//! The shell's [`Launcher`]: the desktop editor through VS Code's `code` command, as `puddle serve`
//! has it, and the browser editor in a window of Puddle's own ([`SandboxWindows`]).
//!
//! The host calls [`Launcher::open_browser`] on an attach in the browser, with where the editor is
//! and a way to its password. The shell signs in on the window's behalf and opens the window; the
//! password goes no further than the sign-in (AT-34). The windows are installed once the backend
//! has started ([`AppLauncher::install`]): their hooks need the backend's problem list, which exists
//! only after the host that holds this launcher.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_api::{BrowserTarget, DesktopTarget, LaunchError, Launcher, WorkspaceRecord};
use puddle_host::VsCodeLauncher;
use puddle_types::WorkspaceName;
use tauri::{AppHandle, Runtime};
use url::Url;

use crate::sandbox_window::{self, SandboxWindows, Target, WindowSettings};

/// How long a delete waits for a closing editor window to let go of its data folder. A window
/// asked to close can take a moment (the editor saves what it holds first).
const CLOSE_WAIT: Duration = Duration::from_secs(5);

/// Opens a workspace's editors for the desktop app.
pub struct AppLauncher<R: Runtime> {
    app: AppHandle<R>,
    /// The folder the windows keep one data folder per workspace in.
    webview_root: PathBuf,
    desktop: VsCodeLauncher,
    windows: OnceLock<SandboxWindows>,
    close_wait: Duration,
}

impl<R: Runtime> std::fmt::Debug for AppLauncher<R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppLauncher").finish_non_exhaustive()
    }
}

impl<R: Runtime> AppLauncher<R> {
    /// A launcher whose windows keep their data under `webview_root`. It opens none until
    /// [`AppLauncher::install`] has given it the windows.
    #[must_use]
    pub fn new(app: AppHandle<R>, webview_root: PathBuf) -> Self {
        Self {
            app,
            webview_root,
            desktop: VsCodeLauncher::new(),
            windows: OnceLock::new(),
            close_wait: CLOSE_WAIT,
        }
    }

    /// The same launcher waiting at most `wait` for windows to close before a delete removes
    /// their data.
    #[must_use]
    pub fn with_close_wait(mut self, wait: Duration) -> Self {
        self.close_wait = wait;
        self
    }

    /// Gives the launcher the windows it opens into. The first call counts; a second changes
    /// nothing.
    pub fn install(&self, windows: SandboxWindows) {
        let _ = self.windows.set(windows);
    }
}

/// The user's message when the window could not be built.
fn not_opened(reason: impl std::fmt::Display) -> LaunchError {
    LaunchError::new(format!("the editor window did not open: {reason}"))
}

impl<R: Runtime> Launcher for AppLauncher<R> {
    fn open_desktop<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        target: &'a DesktopTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        self.desktop.open_desktop(workspace, target)
    }

    fn open_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        target: &'a BrowserTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            let windows = self.windows.get().ok_or_else(|| {
                LaunchError::new("the editor window is not ready yet; try again in a moment")
            })?;
            let origin = Url::parse(&target.origin)
                .map_err(|e| LaunchError::new(format!("the editor's address is not a URL: {e}")))?;
            let cookie = match &target.login {
                Some(login) => {
                    let password = login.password().await?;
                    let session = windows
                        .session(target.front, &workspace.name, &password)
                        .await
                        .map_err(|e| {
                            LaunchError::new(format!(
                                "{e}; open it again, or restart the workspace"
                            ))
                        })?;
                    Some(session)
                }
                None => None,
            };
            let window = Target {
                workspace: workspace.name.clone(),
                origin,
                data_folder: sandbox_window::data_folder(&self.webview_root, &workspace.name),
                settings: WindowSettings::from(&target.settings),
                cookie,
                debug_port: None,
            };
            // Building a window waits for the window system: not on an async worker.
            let (windows, app) = (windows.clone(), self.app.clone());
            let built = tokio::task::spawn_blocking(move || windows.open(&app, &window));
            built
                .await
                .map_err(not_opened)?
                .map(drop)
                .map_err(not_opened)
        })
    }

    fn close_browser<'a>(&'a self, workspace: &'a WorkspaceName) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(windows) = self.windows.get() {
                let asked = windows.close(&self.app, workspace);
                tracing::debug!(%workspace, asked, "editor windows asked to close");
            }
        })
    }

    fn forget_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceName,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            if let Some(windows) = self.windows.get() {
                // A window that is still open holds its data folder (Windows refuses to delete it).
                windows.close(&self.app, workspace);
                let deadline = tokio::time::Instant::now() + self.close_wait;
                while windows.open_count(workspace) > 0 && tokio::time::Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                windows.forget(workspace);
            }
            let (root, name) = (self.webview_root.clone(), workspace.clone());
            // The webview's process can outlive its window by a moment and still hold the folder.
            tokio::task::spawn_blocking(move || {
                let mut removed = sandbox_window::delete_data_folder(&root, &name);
                for _ in 0..10 {
                    if removed.is_ok() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                    removed = sandbox_window::delete_data_folder(&root, &name);
                }
                removed
            })
            .await
            .map_err(|e| LaunchError::new(e.to_string()))?
            .map_err(|e| {
                LaunchError::new(format!(
                    "the editor window's browser data could not be removed: {e}"
                ))
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use http::header::SET_COOKIE;
    use http::{Request, Response};
    use http_body_util::{BodyExt, Full};
    use hyper::service::service_fn;
    use hyper_util::rt::TokioIo;
    use puddle_api::EditorLogin;
    use puddle_types::fixture::password;
    use puddle_types::{WorkspaceId, WorkspaceName};
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
            WorkspaceId::new("w").unwrap(),
            WorkspaceName::new(name).unwrap(),
            Vec::new(),
        )
    }

    fn windows() -> SandboxWindows {
        SandboxWindows::new(Hooks {
            open_external: Arc::new(|_| {}),
            refused: Arc::new(|_| {}),
            last_closed: Arc::new(|_| {}),
        })
    }

    fn target(front: SocketAddr, login: Option<Arc<dyn EditorLogin>>) -> BrowserTarget {
        BrowserTarget {
            origin: format!("http://demo.localhost:{}", front.port()),
            front,
            settings: puddle_settings::resolve(
                &puddle_settings::GlobalSettings::default(),
                None::<&puddle_settings::WorkspaceSettings>,
            ),
            login,
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
        let target = target(addr, Some(Arc::new(Password(secret.clone()))));

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
    async fn forgetting_waits_for_the_closing_window_then_removes_its_data_and_session() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
        let windows = windows();
        launcher.install(windows.clone());
        let secret = password("right");
        let (addr, logins) = front(&secret).await;
        let with_login = target(addr, Some(Arc::new(Password(secret.clone()))));
        launcher
            .open_browser(&record("demo"), &with_login)
            .await
            .unwrap();
        let name = WorkspaceName::new("demo").unwrap();
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
            .forget_browser(&WorkspaceName::new("demo").unwrap())
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
        let name = WorkspaceName::new("demo").unwrap();
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
            .close_browser(&WorkspaceName::new("demo").unwrap())
            .await;
        let err = launcher
            .forget_browser(&WorkspaceName::new("demo").unwrap())
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("the editor window's browser data could not be removed"),
            "{err}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn microsoft_s_server_opens_with_no_login() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let launcher = AppLauncher::new(app.handle().clone(), data.path().join("webview"));
        launcher.install(windows());
        let (addr, logins) = front(&password("unused")).await;
        launcher
            .open_browser(&record("demo"), &target(addr, None))
            .await
            .unwrap();
        assert!(app.get_webview_window("vs-demo").is_some());
        assert!(logins.lock().unwrap().is_empty());
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
                &target(addr, Some(Arc::new(Password(password("wrong"))))),
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
}
