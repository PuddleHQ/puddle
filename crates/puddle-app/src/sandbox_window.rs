// SPDX-License-Identifier: AGPL-3.0-or-later
//! A workspace's browser editor window.
//!
//! The editor is a web page served by the guest, so it is hostile as far as the shell is
//! concerned. The window gives it a browser and nothing else:
//!
//! * **No app permissions.** The label `vs-<workspace>` is named by no capability file, so every
//!   command is refused (`tests/acl.rs` shows it for these windows too).
//! * **One origin.** Navigation stays on `<workspace>.localhost:<port>`; web addresses elsewhere
//!   go to the default browser, called from Rust; everything else is dropped.
//! * **No pop-up of the webview's own.** The editor's "New Window" gets another guarded window of
//!   this kind. Everything else a page opens goes to the default browser or is dropped, and the
//!   editor's `/proxy/<port>/` links open a guarded window of this kind on Puddle's own forward of
//!   that port, `<port>.<workspace>.localhost` ([`forward_url`]), with a data folder of its own: the
//!   page there is the guest's own app, not the editor. The answer to a new-window request is never
//!   `Allow`, which would be the webview's own pop-up with none of these guards.
//! * **A writable `Object.prototype`.** Tauri's `freezePrototype` is off for the app, because the
//!   editor's own scripts assign to built-in prototypes. The page is the untrusted side here, so a
//!   freeze would guard nothing; the app's main window freezes its own (`window::init_script`).
//! * **A data folder per workspace,** kept between opens and deleted with the workspace.
//! * **The editor's credential before it navigates:** the window starts on a blank page, gets the
//!   session cookie from [`signin`] and then navigates; the credential is never in a URL.
//! * **No developer tools, no browser shortcuts** (Windows), zoom keys and clipboard reads as the
//!   workspace's settings say.
//!
//! A window is built off the main thread (setting the cookie waits for the webview).
//! The window-system glue is thin on purpose: each handler hands over to a function in this file
//! that unit tests call. Only a real webview shows the handlers being called, which the desktop
//! shell's smoke test does (`e2e/smoke.mjs`).

pub mod signin;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use puddle_api::OpenTarget;
use puddle_settings::{ClipboardRead, Effective};
use puddle_types::WorkspaceId;
use tauri::webview::{
    NewWindowFeatures, NewWindowResponse, PermissionKind, PermissionResponse, cookie::Cookie,
};
use tauri::{
    AppHandle, Manager, Runtime, Webview, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
    WindowEvent,
};
use url::Url;

use crate::navigation::{Navigation, OriginPolicy};
use crate::window::{OpenExternal, Refused, navigation_handler};

pub use signin::SessionCookie;

/// Every workspace window's label starts with this; the main window's label never does.
pub const LABEL_PREFIX: &str = "vs-";

/// The label of the first window of `workspace`.
#[must_use]
pub fn label(workspace: &WorkspaceId) -> String {
    format!("{LABEL_PREFIX}{workspace}")
}

/// Whether `label` belongs to a workspace window. The shell does not remember such a window's
/// place (the main window's is remembered).
#[must_use]
pub fn is_sandbox_label(label: &str) -> bool {
    label.starts_with(LABEL_PREFIX)
}

/// Whether the shell remembers the size and place of the window `label` between runs: the main
/// window's, not a workspace window's (its place would follow a workspace that may be gone).
#[must_use]
pub fn remembers_place(label: &str) -> bool {
    !is_sandbox_label(label)
}

/// The label of the `n`th further window of `workspace` (the editor's "New Window"). A colon cannot
/// be part of a workspace name, so no workspace's first window can have this label.
fn popup_label(workspace: &WorkspaceId, n: usize) -> String {
    format!("{LABEL_PREFIX}{workspace}:{n}")
}

/// Whether `label` is a window of `workspace`, the first or a further one.
fn belongs_to(label: &str, workspace: &WorkspaceId) -> bool {
    let own = self::label(workspace);
    label == own
        || label
            .strip_prefix(&own)
            .is_some_and(|rest| rest.starts_with(':'))
}

/// The folder of the webview's data (cookies, storage, the editor's unsaved work) for `workspace`
/// under `webview_root`. Each workspace has its own, so nothing is shared between two editors.
#[must_use]
pub fn data_folder(webview_root: &Path, workspace: &WorkspaceId) -> PathBuf {
    webview_root.join(workspace.as_str())
}

/// Whether the webview keeps data of `workspace` (the editor's or its forwarded ports'). A folder
/// that cannot be looked at counts as there.
#[must_use]
pub fn has_data_folder(webview_root: &Path, workspace: &WorkspaceId) -> bool {
    let editor = data_folder(webview_root, workspace);
    let forward = forward_data_folder(&editor);
    [editor, forward]
        .iter()
        .any(|folder| !matches!(folder.try_exists(), Ok(false)))
}

/// Deletes the webview's data of `workspace` (the editor's and its forwarded ports'), when the
/// workspace is deleted. A folder that is not there is fine.
///
/// # Errors
///
/// A folder exists and could not be removed (a window still has it open). The other one is still
/// tried.
pub fn delete_data_folder(webview_root: &Path, workspace: &WorkspaceId) -> std::io::Result<()> {
    let editor = data_folder(webview_root, workspace);
    let forward = forward_data_folder(&editor);
    let gone = |folder: &Path| match std::fs::remove_dir_all(folder) {
        Err(err) if err.kind() != std::io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    };
    let editor = gone(&editor);
    gone(&forward).and(editor)
}

/// What the workspace's settings say about its windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowSettings {
    /// Ctrl and plus/minus zoom the page.
    pub zoom_hotkeys: bool,
    /// What a page that reads the clipboard by script gets.
    pub clipboard_read: ClipboardRead,
}

impl From<&Effective> for WindowSettings {
    fn from(effective: &Effective) -> Self {
        Self {
            zoom_hotkeys: effective.zoom_hotkeys.value,
            clipboard_read: effective.clipboard_read.value,
        }
    }
}

/// What a request for a new window does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewWindow {
    /// Open another guarded window of this kind (the editor's "New Window").
    Create,
    /// Open a guarded window on the address, which is Puddle's own forward of a guest port (the
    /// editor's `/proxy/<port>/` link, rewritten by [`forward_url`]).
    Forward(Url),
    /// Open the address in the default browser.
    External,
    /// Do nothing; the user is told when it was a link.
    Refuse,
}

/// The address of Puddle's own forward for an editor link `/proxy/<port>/<rest>` or
/// `/absproxy/<port>/<rest>`: the same address with the host `<port>.<workspace>.localhost`. The
/// `/proxy/` prefix goes (code-server's own proxy strips it too); `/absproxy/` keeps its path,
/// because the app is configured to serve under it. `None` when the link names no port: digits
/// only, no leading zero, 1 to 65535.
#[must_use]
pub fn forward_url(link: &Url) -> Option<Url> {
    let path = link.path();
    let (rest, keep_path) = match path.strip_prefix("/proxy/") {
        Some(rest) => (rest, false),
        None => (path.strip_prefix("/absproxy/")?, true),
    };
    let (port, tail) = rest.split_once('/').unwrap_or((rest, ""));
    let valid = port.bytes().all(|b| b.is_ascii_digit())
        && !port.starts_with('0')
        && port.parse::<u16>().is_ok();
    if !valid {
        return None;
    }
    let mut url = link.clone();
    url.set_host(Some(&format!("{port}.{}", link.host_str()?)))
        .ok()?;
    if !keep_path {
        url.set_path(&format!("/{tail}"));
    }
    Some(url)
}

/// Classifies a request for a new window. Only the editor's own pages (same origin, not behind the
/// guest's port proxy) become windows; the port proxy's links go to Puddle's own forward.
#[must_use]
pub fn classify_new_window(policy: &OriginPolicy, target: &Url) -> NewWindow {
    classify(policy, target, true)
}

/// [`classify_new_window`], where `proxy_links` says whether `/proxy/` paths are the editor's port
/// proxy. On a forwarded app's origin they are the app's own paths.
fn classify(policy: &OriginPolicy, target: &Url, proxy_links: bool) -> NewWindow {
    let proxied = proxy_links
        && (target.path().starts_with("/proxy/") || target.path().starts_with("/absproxy/"));
    match policy.classify(target) {
        // `about:blank` and `blob:` are "stay" for navigation but are not a workbench page.
        Navigation::Stay if !matches!(target.scheme(), "http" | "https") => NewWindow::Refuse,
        Navigation::Stay if proxied => {
            forward_url(target).map_or(NewWindow::External, NewWindow::Forward)
        }
        Navigation::Stay => NewWindow::Create,
        Navigation::External => NewWindow::External,
        Navigation::Refuse => NewWindow::Refuse,
    }
}

/// The answer to a permission request of the page. Only a script's clipboard read is decided here:
/// the settings say allow or deny. "Ask" leaves it to the webview's own prompt, which keeps the answer in
/// the workspace's own data folder, so the question is asked once per workspace. Everything else
/// stays at the webview's default.
#[must_use]
pub fn permission_response(settings: &WindowSettings, kind: &PermissionKind) -> PermissionResponse {
    match (kind, settings.clipboard_read) {
        (PermissionKind::ClipboardRead, ClipboardRead::Allow) => PermissionResponse::Allow,
        (PermissionKind::ClipboardRead, ClipboardRead::Deny) => PermissionResponse::Deny,
        _ => PermissionResponse::Default,
    }
}

/// Where a workspace's editor opens and how its window is set up.
#[derive(Debug, Clone)]
pub struct Target {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// What the user calls the workspace: shown in the window's title, never used to find
    /// anything.
    pub display_name: String,
    /// The editor's origin, `http://<workspace>.localhost:<port>/` (the front's `origin`).
    pub origin: Url,
    /// The webview's data folder ([`data_folder`]).
    pub data_folder: PathBuf,
    /// The workspace's window settings.
    pub settings: WindowSettings,
    /// The cookie that signs the window in: code-server's session ([`signin::sign_in`]) or
    /// Microsoft's server's connection token ([`SessionCookie::connection_token`]).
    pub cookie: Option<SessionCookie>,
    /// A port for WebView2's DevTools Protocol, for the smoke test (Windows only; the window is
    /// otherwise built as for a user).
    pub debug_port: Option<u16>,
}

/// Reports the end of a request for another window of a workspace (see [`Hooks::new_window`]).
pub type NewWindowReport = Arc<dyn Fn(&WorkspaceId, Result<(), &str>) + Send + Sync>;

/// What the windows ask of the shell.
#[derive(Clone)]
pub struct Hooks {
    /// Hands a web address to the default browser.
    pub open_external: OpenExternal,
    /// Tells the user a link was refused.
    pub refused: Refused,
    /// Tells the user how a request for another window of a workspace ended: `Err` carries why
    /// nothing opened, `Ok` withdraws an earlier notice.
    pub new_window: NewWindowReport,
    /// Called when a workspace's last window is gone, so the guest can end the editor's extension
    /// hosts. Called from the window system's thread: it must not wait.
    pub last_closed: Arc<dyn Fn(&WorkspaceId) + Send + Sync>,
}

impl std::fmt::Debug for Hooks {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hooks").finish_non_exhaustive()
    }
}

/// The open workspace windows, and the opening and closing of them.
#[derive(Clone, Debug)]
pub struct SandboxWindows {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    hooks: Hooks,
    /// Windows open per workspace.
    open: Mutex<HashMap<WorkspaceId, usize>>,
    /// Windows open on forwarded ports per workspace. They end nothing, but they hold the
    /// `<workspace>.forward` data folder, so deleting the workspace waits for them.
    forward_open: Mutex<HashMap<WorkspaceId, usize>>,
    /// Source of the popup labels.
    popups: AtomicUsize,
    /// Set once the quit has begun: the windows' ends no longer run the hook, because the quit
    /// asks the guests itself and waits for them.
    quitting: AtomicBool,
    /// The cookie the last sign-in gave a workspace, with the password it was for: the editor's
    /// login limits how often one can sign in, and a window reopened a moment later must not hit it.
    sessions: Mutex<HashMap<WorkspaceId, (zeroize::Zeroizing<String>, SessionCookie)>>,
}

impl SandboxWindows {
    /// A registry with no window open.
    #[must_use]
    pub fn new(hooks: Hooks) -> Self {
        Self {
            inner: Arc::new(Inner {
                hooks,
                open: Mutex::new(HashMap::new()),
                forward_open: Mutex::new(HashMap::new()),
                popups: AtomicUsize::new(0),
                quitting: AtomicBool::new(false),
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// How many windows `workspace` has open.
    #[must_use]
    pub fn open_count(&self, workspace: &WorkspaceId) -> usize {
        self.inner
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .copied()
            .unwrap_or(0)
    }

    /// How many windows on forwarded ports `workspace` has open.
    #[must_use]
    pub fn forward_open_count(&self, workspace: &WorkspaceId) -> usize {
        self.inner
            .forward_open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn forward_opened(&self, workspace: &WorkspaceId) {
        *self
            .inner
            .forward_open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace.clone())
            .or_insert(0) += 1;
    }

    fn forward_closed(&self, workspace: &WorkspaceId) {
        let mut open = self
            .inner
            .forward_open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        open.entry(workspace.clone())
            .and_modify(|count| *count -= 1);
        open.retain(|_, count| *count > 0);
    }

    /// Starts the quit: from now on a window's end does not run the last-closed hook, and the
    /// windows count as gone. Says which workspaces had an editor window open, sorted, for the
    /// caller to clean up after closing the windows; a second call says none. A window opened
    /// after this is not covered.
    pub fn begin_quit(&self) -> Vec<WorkspaceId> {
        self.inner.quitting.store(true, Ordering::SeqCst);
        let mut open: Vec<_> = std::mem::take(
            &mut *self
                .inner
                .open
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        )
        .into_keys()
        .collect();
        open.sort();
        open
    }

    pub(crate) fn opened(&self, workspace: &WorkspaceId) {
        *self
            .inner
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace.clone())
            .or_insert(0) += 1;
    }

    /// A window of `workspace` is gone. The hook runs when it was the last one.
    fn closed(&self, workspace: &WorkspaceId) {
        let last = {
            let mut open = self
                .inner
                .open
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match open.get_mut(workspace) {
                Some(count) if *count > 1 => {
                    *count -= 1;
                    false
                }
                Some(_) => {
                    open.remove(workspace);
                    true
                }
                None => false,
            }
        };
        if last && !self.inner.quitting.load(Ordering::SeqCst) {
            (self.inner.hooks.last_closed)(workspace);
        }
    }

    /// The session cookie for `workspace`, signing in only when the last one was for another
    /// password.
    ///
    /// # Errors
    ///
    /// [`signin::SignInError`] when the editor does not give a session.
    pub async fn session(
        &self,
        front: std::net::SocketAddr,
        workspace: &WorkspaceId,
        password: &str,
    ) -> Result<SessionCookie, signin::SignInError> {
        if let Some((for_password, cookie)) = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            && **for_password == *password
        {
            return Ok(cookie.clone());
        }
        let cookie = signin::sign_in(front, workspace, password).await?;
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(
                workspace.clone(),
                (zeroize::Zeroizing::new(password.to_owned()), cookie.clone()),
            );
        Ok(cookie)
    }

    /// Forgets what is kept for `workspace` (its session), when the workspace is deleted.
    pub fn forget(&self, workspace: &WorkspaceId) {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(workspace);
    }

    /// Opens the editor window of `target`, or brings its window to the front when it is open.
    /// Call it off the main thread: the cookie is set through the webview, which waits for the
    /// window system.
    ///
    /// # Errors
    ///
    /// Whatever Tauri reports when the window or its data folder can't be created.
    pub fn open<R: Runtime, M: Manager<R>>(
        &self,
        manager: &M,
        target: &Target,
    ) -> tauri::Result<WebviewWindow<R>> {
        self.open_at(manager, target, None)
    }

    /// [`Self::open`] on the place the editor should show (`open`), which only [`start_url`]
    /// turns into an address; `None` is the server's own start page. A window that is already
    /// open keeps what it shows.
    ///
    /// # Errors
    ///
    /// As [`Self::open`].
    pub fn open_at<R: Runtime, M: Manager<R>>(
        &self,
        manager: &M,
        target: &Target,
        open: Option<&OpenTarget>,
    ) -> tauri::Result<WebviewWindow<R>> {
        let label = label(&target.workspace);
        if let Some(existing) = manager.get_webview_window(&label) {
            // Both can fail when the window is being closed; there is nothing to do about that.
            let _ = existing.unminimize();
            let _ = existing.set_focus();
            return Ok(existing);
        }
        std::fs::create_dir_all(&target.data_folder)?;
        let target = Arc::new(target.clone());
        // With a cookie to set, the window starts blank and goes to the editor once it has it.
        let editor = open.map_or_else(|| target.origin.clone(), |o| start_url(&target.origin, o));
        let start = if target.cookie.is_some() {
            blank(&target.origin)
        } else {
            editor.clone()
        };
        let app = manager.app_handle();
        let window = self.build(app, &target, &label, WebviewUrl::External(start), None)?;
        if let Some(cookie) = &target.cookie {
            if let Err(err) = window.set_cookie(session_cookie(&target, cookie)) {
                // The editor's own login page shows instead; the user can sign in there.
                tracing::warn!(%err, "the editor's session could not be set; its login page shows");
            }
            window.navigate(editor)?;
        }
        Ok(window)
    }

    /// The open windows of `workspace`, the first and any further ones.
    fn windows_of<R: Runtime, M: Manager<R>>(
        manager: &M,
        workspace: &WorkspaceId,
    ) -> Vec<WebviewWindow<R>> {
        manager
            .webview_windows()
            .into_iter()
            .filter(|(label, _)| belongs_to(label, workspace))
            .map(|(_, window)| window)
            .collect()
    }

    /// Closes every window of `workspace` the way a user would, so the editor disconnects cleanly.
    /// The close is asked for, not waited for; the last window's end runs the hook. Says how many
    /// windows were asked.
    pub fn close<R: Runtime, M: Manager<R>>(&self, manager: &M, workspace: &WorkspaceId) -> usize {
        let windows = Self::windows_of(manager, workspace);
        for window in &windows {
            // A window that is already going away can't be closed again.
            let _ = window.close();
        }
        windows.len()
    }

    /// Builds one window: the first of a workspace, or one the editor asked for (`features` set).
    fn build<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        target: &Arc<Target>,
        label: &str,
        start: WebviewUrl,
        features: Option<NewWindowFeatures>,
    ) -> tauri::Result<WebviewWindow<R>> {
        let policy = OriginPolicy::new(&target.origin, None);
        let hooks = &self.inner.hooks;
        let settings = target.settings;
        let mut builder = WebviewWindowBuilder::new(app, label, start)
            .title(window_title(&target.display_name))
            .inner_size(1400.0, 900.0)
            .zoom_hotkeys_enabled(settings.zoom_hotkeys)
            .devtools(false)
            .on_navigation(navigation_handler(
                policy.clone(),
                hooks.open_external.clone(),
                hooks.refused.clone(),
            ))
            .on_permission_request(permission_handler(settings));
        builder = match features {
            // A further window shares the opener's webview environment (and with it the data
            // folder); a second folder for it would not be allowed.
            Some(features) => builder.window_features(features),
            None => builder.data_directory(target.data_folder.clone()),
        };
        #[cfg(windows)]
        if let Some(port) = target.debug_port {
            builder = builder.additional_browser_args(&debug_arguments(port));
        }
        #[cfg(not(windows))]
        let _ = target.debug_port;
        let (this, app_for_popups, popup_target) = (self.clone(), app.clone(), Arc::clone(target));
        let builder = builder.on_new_window(move |url, features| {
            this.new_window(&app_for_popups, &popup_target, &url, Some(features))
        });
        // The cleanup hook ends the editor's extension hosts when the last editor window is gone,
        // whatever apps are still open on forwarded ports, so those are counted apart.
        let forward = is_forward(target);
        if forward {
            self.forward_opened(&target.workspace);
        } else {
            self.opened(&target.workspace);
        }
        let window = match builder.build() {
            Ok(window) => window,
            Err(err) => {
                if forward {
                    self.forward_closed(&target.workspace);
                } else {
                    self.closed(&target.workspace);
                }
                return Err(err);
            }
        };
        if forward {
            window.on_window_event(self.on_forward_event(target.workspace.clone()));
        } else {
            window.on_window_event(self.on_event(target.workspace.clone()));
        }
        #[cfg(windows)]
        crate::accelerators::turn_off(&window);
        Ok(window)
    }

    /// What a window of `workspace` does with its events: the end of the window is counted.
    pub(crate) fn on_event(
        &self,
        workspace: WorkspaceId,
    ) -> impl Fn(&WindowEvent) + Send + 'static {
        let this = self.clone();
        move |event| {
            if matches!(event, WindowEvent::Destroyed) {
                this.closed(&workspace);
            }
        }
    }

    /// What a window on a forwarded port does with its events: its end is counted, and nothing else.
    pub(crate) fn on_forward_event(
        &self,
        workspace: WorkspaceId,
    ) -> impl Fn(&WindowEvent) + Send + 'static {
        let this = self.clone();
        move |event| {
            if matches!(event, WindowEvent::Destroyed) {
                this.forward_closed(&workspace);
            }
        }
    }

    /// The answer to a page's request for a new window. `features` are the opener's, which the
    /// new window shares the webview environment through; only a test has none.
    fn new_window<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        target: &Arc<Target>,
        url: &Url,
        features: Option<NewWindowFeatures>,
    ) -> NewWindowResponse<R> {
        let hooks = &self.inner.hooks;
        let policy = OriginPolicy::new(&target.origin, None);
        match classify(&policy, url, !is_forward(target)) {
            NewWindow::Create => {
                let n = self.inner.popups.fetch_add(1, Ordering::Relaxed);
                let start = WebviewUrl::External(blank(&target.origin));
                match self.build(
                    app,
                    target,
                    &popup_label(&target.workspace, n),
                    start,
                    features,
                ) {
                    Ok(window) => {
                        (hooks.new_window)(&target.workspace, Ok(()));
                        NewWindowResponse::Create { window }
                    }
                    Err(err) => {
                        tracing::warn!(%err, "a new editor window could not be built");
                        // The user clicked New Window: say that nothing opened, and why.
                        (hooks.new_window)(&target.workspace, Err(&err.to_string()));
                        NewWindowResponse::Deny
                    }
                }
            }
            NewWindow::Forward(address) => {
                // Not `Create`: a pop-up would first load the editor's `/proxy/` address, which a
                // window on the forwarded origin sends to the default browser.
                if let Err(err) = self.open_forward(app, target, &address) {
                    tracing::warn!(%err, "a window on a forwarded port could not be built");
                    (hooks.refused)(url);
                }
                NewWindowResponse::Deny
            }
            NewWindow::External => {
                (hooks.open_external)(url);
                NewWindowResponse::Deny
            }
            NewWindow::Refuse => {
                (hooks.refused)(url);
                NewWindowResponse::Deny
            }
        }
    }

    /// Opens a window on `address`, Puddle's forward of a guest port, with a data folder of its own
    /// inside the workspace's, so it goes with the workspace.
    fn open_forward<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        target: &Target,
        address: &Url,
    ) -> tauri::Result<()> {
        let forwarded = Arc::new(forward_target(target, address));
        std::fs::create_dir_all(&forwarded.data_folder)?;
        let n = self.inner.popups.fetch_add(1, Ordering::Relaxed);
        let label = popup_label(&target.workspace, n);
        self.build(
            app,
            &forwarded,
            &label,
            WebviewUrl::External(address.clone()),
            None,
        )?;
        Ok(())
    }
}

/// A window's title: the workspace's display name, never its id.
fn window_title(display_name: &str) -> String {
    format!("{display_name} - Puddle")
}

/// The target of a window on `address`, a forward of one of `editor`'s workspace's guest ports.
fn forward_target(editor: &Target, address: &Url) -> Target {
    let mut origin = address.clone();
    origin.set_path("/");
    origin.set_query(None);
    origin.set_fragment(None);
    Target {
        origin,
        data_folder: forward_data_folder(&editor.data_folder),
        cookie: None,
        // The page is the guest's own app: the setting that lets the editor read the clipboard
        // unasked does not extend to it.
        settings: WindowSettings {
            clipboard_read: match editor.settings.clipboard_read {
                ClipboardRead::Allow => ClipboardRead::Ask,
                other => other,
            },
            ..editor.settings
        },
        // Its own data folder means its own WebView2 browser process, which cannot open the
        // DevTools port the editor's process already holds.
        debug_port: editor.debug_port.and_then(|port| port.checked_add(1)),
        ..editor.clone()
    }
}

/// The data folder of the windows on a workspace's forwarded ports, beside the editor's
/// `editor_folder` (not inside it: two webview environments do not share a folder tree). A dot
/// cannot be part of a workspace name, so no workspace's own folder has this name.
fn forward_data_folder(editor_folder: &Path) -> PathBuf {
    let mut name = editor_folder.file_name().unwrap_or_default().to_owned();
    name.push(".forward");
    editor_folder.with_file_name(name)
}

/// Whether `target` is a window on a forwarded port rather than on the editor: its host has the
/// port in front of `<workspace>.localhost`.
fn is_forward(target: &Target) -> bool {
    target.origin.host_str() != Some(&format!("{}.localhost", target.workspace))
}

/// The address a window opens a place on: the editor's own page with the place in its query,
/// `/?workspace=<file>` for the multi-root file and `/?folder=<path>` for a folder, the form both
/// servers read. Nothing else is built: the path is always `/`, the query has this one key, and
/// the path in the value is encoded, so it can't add a key, a path or a fragment.
#[must_use]
pub fn start_url(origin: &Url, open: &OpenTarget) -> Url {
    let mut url = origin.clone();
    url.set_path("/");
    url.set_fragment(None);
    let key = if open.is_workspace_file {
        "workspace"
    } else {
        "folder"
    };
    url.query_pairs_mut().clear().append_pair(key, &open.path);
    url
}

/// The blank page a window starts on before it has anything to show. `fallback` is only there
/// because parsing returns a `Result`.
fn blank(fallback: &Url) -> Url {
    Url::parse("about:blank").unwrap_or(fallback.clone())
}

/// The permission callback of a window with `settings`.
fn permission_handler<R: Runtime>(
    settings: WindowSettings,
) -> impl Fn(Webview<R>, PermissionKind) -> PermissionResponse + Send + Sync + 'static {
    move |_, kind| permission_response(&settings, &kind)
}

/// The cookie to set in the window: the editor's session, for the editor's host name only.
fn session_cookie(target: &Target, cookie: &SessionCookie) -> Cookie<'static> {
    Cookie::build((cookie.name().to_owned(), cookie.value().to_owned()))
        // No leading dot: WebView2 then makes a cookie for this host only, not its subdomains.
        .domain(format!("{}.localhost", target.workspace))
        .path("/")
        .http_only(!cookie.script_reads())
        // Not `Secure`: this is an http loopback origin, and WebKitGTK (2.54, libsoup 3.6) drops a
        // Secure cookie for `http://<name>.localhost` instead of sending it, which would break
        // sign-in. SameSite stays, and HttpOnly unless the page's script must read the cookie; no
        // network path reaches a loopback origin.
        .same_site(tauri::webview::cookie::SameSite::Lax)
        .build()
}

/// WebView2's arguments with the DevTools Protocol open on `port`; the first two are wry's own
/// defaults (an argument string replaces them).
#[cfg(windows)]
fn debug_arguments(port: u16) -> String {
    format!(
        "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --remote-debugging-port={port} --remote-allow-origins=*"
    )
}

#[cfg(test)]
mod tests;
