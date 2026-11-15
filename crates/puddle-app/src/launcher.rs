// SPDX-License-Identifier: AGPL-3.0-or-later
//! The shell's [`Launcher`]: the desktop editor through VS Code's `code` command, as `puddle serve`
//! has it, and the browser editor in a window of Puddle's own ([`SandboxWindows`]).
//!
//! The host calls [`Launcher::open_browser`] on an attach in the browser, with where the editor is
//! and a way to its password. The shell signs in on the window's behalf and opens the window; the
//! password goes no further than the sign-in. The windows are installed once the backend
//! has started ([`AppLauncher::install`]): their hooks need the backend's problem list, which exists
//! only after the host that holds this launcher.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use futures_util::future::BoxFuture;
use puddle_api::{
    BrowserTarget, DesktopTarget, EditorSignIn, LaunchError, Launcher, WorkspaceRecord,
};
use puddle_host::VsCodeLauncher;
use puddle_types::WorkspaceId;
use tauri::{AppHandle, Runtime};
use url::Url;

use crate::sandbox_window::{self, SandboxWindows, SessionCookie, Target, WindowSettings};

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
                Some(EditorSignIn::Form(login)) => {
                    let password = login.password().await?;
                    let session = windows
                        .session(target.front, &workspace.id, &password)
                        .await
                        .map_err(|e| {
                            LaunchError::new(format!(
                                "{e}; open it again, or restart the workspace"
                            ))
                        })?;
                    Some(session)
                }
                Some(EditorSignIn::ConnectionToken(login)) => {
                    Some(SessionCookie::connection_token(login.password().await?))
                }
                None => None,
            };
            let window = Target {
                workspace: workspace.id.clone(),
                display_name: workspace.label().to_owned(),
                origin,
                data_folder: sandbox_window::data_folder(&self.webview_root, &workspace.id),
                settings: WindowSettings::from(&target.settings),
                cookie,
                debug_port: None,
            };
            // Building a window waits for the window system: not on an async worker.
            let (windows, app) = (windows.clone(), self.app.clone());
            let open = target.open.clone();
            let built =
                tokio::task::spawn_blocking(move || windows.open_at(&app, &window, open.as_ref()));
            built
                .await
                .map_err(not_opened)?
                .map(drop)
                .map_err(not_opened)
        })
    }

    fn close_browser<'a>(&'a self, workspace: &'a WorkspaceId) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(windows) = self.windows.get() {
                let asked = windows.close(&self.app, workspace);
                tracing::debug!(%workspace, asked, "editor windows asked to close");
            }
        })
    }

    fn has_browser_data(&self, workspace: &WorkspaceId) -> bool {
        sandbox_window::has_data_folder(&self.webview_root, workspace)
    }

    fn forget_browser<'a>(
        &'a self,
        workspace: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async move {
            if let Some(windows) = self.windows.get() {
                // A window that is still open holds its data folder (Windows refuses to delete it).
                windows.close(&self.app, workspace);
                let deadline = tokio::time::Instant::now() + self.close_wait;
                // Editor windows and windows on forwarded ports both hold a data folder.
                let held = || windows.open_count(workspace) + windows.forward_open_count(workspace);
                while held() > 0 && tokio::time::Instant::now() < deadline {
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
mod tests;
