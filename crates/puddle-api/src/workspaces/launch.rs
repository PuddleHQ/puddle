// SPDX-License-Identifier: AGPL-3.0-or-later
//! Opening a workspace in an editor: what the launcher returns and how sign-in is passed on.

use std::fmt;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use puddle_types::WorkspaceId;

use super::WorkspaceRecord;

/// Opens VS Code on the desktop for a workspace; the desktop shell implements it.
pub trait Launcher: Send + Sync {
    /// Opens an editor window attached to the workspace.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] with a message for the user (VS Code is not installed, the remote
    /// extension is missing, ...).
    fn open_desktop<'a>(
        &'a self,
        workspace: &'a WorkspaceRecord,
        target: &'a DesktopTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>>;

    /// Opens the workspace's browser editor in a window of the desktop shell. The default is for
    /// a launcher with no window (`puddle serve`): it says so, because the editor's password is
    /// handed to a window the shell controls and to nothing else.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] with a message for the user (no window here, the editor gave no session,
    /// the window could not be built).
    fn open_browser<'a>(
        &'a self,
        _workspace: &'a WorkspaceRecord,
        _target: &'a BrowserTarget,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async {
            Err(LaunchError::new(
                "the browser editor opens from the desktop app",
            ))
        })
    }

    /// Closes the workspace's editor windows the way a user would, because its guest is about to
    /// stop or go. Asks and returns: a window that is slow to close is not waited for. The default
    /// has no window.
    fn close_browser<'a>(&'a self, _workspace: &'a WorkspaceId) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    /// Removes what the shell kept for a workspace that is deleted: its windows' browser data and
    /// sign-in. A workspace made later under the same name must start with none of it.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] with a message for the user when something could not be removed (a window
    /// still holds it).
    fn forget_browser<'a>(
        &'a self,
        _workspace: &'a WorkspaceId,
    ) -> BoxFuture<'a, Result<(), LaunchError>> {
        Box::pin(async { Ok(()) })
    }

    /// Whether the shell keeps browser data under `workspace`'s id (a window's data folder). A
    /// new workspace never gets an id that has some. The default keeps none.
    fn has_browser_data(&self, _workspace: &WorkspaceId) -> bool {
        false
    }
}

/// What a browser editor window needs: where the editor is and how to sign in to it.
#[derive(Clone)]
pub struct BrowserTarget {
    /// The editor's origin on the front, `http://<workspace>.localhost:<port>`.
    pub origin: String,
    /// The front's address, for the sign-in.
    pub front: std::net::SocketAddr,
    /// The workspace's settings, of which a window uses zoom and the clipboard prompt.
    pub settings: puddle_settings::Effective,
    /// How the window signs in to the editor server; `None` when no server is known to run.
    pub login: Option<EditorSignIn>,
    /// What the window opens in the running server, the same place a desktop editor opens; `None`
    /// when the workspace has no running guest to name one, and the window opens the server's own
    /// start page.
    pub open: Option<OpenTarget>,
}

/// A place in a running workspace an editor opens: a checkout, the volume root, or the multi-root
/// file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTarget {
    /// The absolute path in the guest.
    pub path: String,
    /// Whether `path` is a `.code-workspace` file rather than a folder.
    pub is_workspace_file: bool,
}

/// How a window signs in to a workspace's editor server. Either way the window carries the
/// editor's password (or what it buys) in a cookie set before it navigates, never in a URL.
#[derive(Clone)]
pub enum EditorSignIn {
    /// code-server: the password goes in a login form POST to `/login` on the editor's origin, and
    /// the window gets the session cookie the answer sets.
    Form(Arc<dyn EditorLogin>),
    /// Microsoft's server: the password is its connection token, which the window carries in the
    /// cookie [`CONNECTION_TOKEN_COOKIE`]. The server refuses every request without it, and the
    /// workbench's script reads it from the cookie for its socket's handshake, so it can't be
    /// `HttpOnly`.
    ConnectionToken(Arc<dyn EditorLogin>),
}

/// The cookie Microsoft's server reads its connection token from.
pub const CONNECTION_TOKEN_COOKIE: &str = "vscode-tkn";

impl EditorSignIn {
    /// The way to the password, whichever the use.
    #[must_use]
    pub fn reader(&self) -> &Arc<dyn EditorLogin> {
        match self {
            Self::Form(login) | Self::ConnectionToken(login) => login,
        }
    }
}

impl fmt::Debug for BrowserTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BrowserTarget")
            .field("origin", &self.origin)
            .field("front", &self.front)
            .finish_non_exhaustive()
    }
}

/// The editor's password, read when a window signs in. Never log what it returns.
pub trait EditorLogin: Send + Sync {
    /// The password of this boot of the workspace's editor server.
    ///
    /// # Errors
    ///
    /// [`LaunchError`] when the workspace is not running or its agent has none yet.
    fn password(&self) -> BoxFuture<'_, Result<String, LaunchError>>;
}

/// What a desktop editor opens: a place in a workspace, reached through its ssh host entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopTarget {
    /// The workspace's host name in the ssh config (`puddle-<workspace>`).
    pub ssh_host: String,
    /// The absolute path in the guest to open: a checkout, the volume root, or the multi-root
    /// file.
    pub path: String,
    /// Whether `path` is a `.code-workspace` file rather than a folder.
    pub is_workspace_file: bool,
}

impl DesktopTarget {
    /// The `vscode-remote` URI of the place (`--folder-uri` or `--file-uri` take it), on the
    /// workspace's ssh host; the path is percent-encoded.
    #[must_use]
    pub fn remote_uri(&self) -> String {
        use std::fmt::Write as _;
        let mut path = String::new();
        for byte in self.path.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.' | b'~') {
                path.push(char::from(byte));
            } else {
                let _ = write!(path, "%{byte:02X}");
            }
        }
        format!("vscode-remote://ssh-remote+{}{path}", self.ssh_host)
    }

    /// The link that opens the place from another program, which VS Code registers as the handler
    /// of `vscode://`: what a browser on the user's own computer follows when the host cannot open
    /// an editor there itself.
    #[must_use]
    pub fn open_uri(&self) -> String {
        let remote = self.remote_uri();
        let rest = remote.strip_prefix("vscode-remote://").unwrap_or(&remote);
        format!("vscode://vscode-remote/{rest}")
    }
}

/// The desktop editor could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct LaunchError {
    message: String,
}

impl LaunchError {
    /// An error with this message for the user.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}
