// SPDX-License-Identifier: GPL-3.0-or-later
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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use puddle_settings::{ClipboardRead, Effective};
use puddle_types::WorkspaceName;
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
pub fn label(workspace: &WorkspaceName) -> String {
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
fn popup_label(workspace: &WorkspaceName, n: usize) -> String {
    format!("{LABEL_PREFIX}{workspace}:{n}")
}

/// Whether `label` is a window of `workspace`, the first or a further one.
fn belongs_to(label: &str, workspace: &WorkspaceName) -> bool {
    let own = self::label(workspace);
    label == own
        || label
            .strip_prefix(&own)
            .is_some_and(|rest| rest.starts_with(':'))
}

/// The folder of the webview's data (cookies, storage, the editor's unsaved work) for `workspace`
/// under `webview_root`. Each workspace has its own, so nothing is shared between two editors.
#[must_use]
pub fn data_folder(webview_root: &Path, workspace: &WorkspaceName) -> PathBuf {
    webview_root.join(workspace.as_str())
}

/// Deletes the webview's data of `workspace` (the editor's and its forwarded ports'), when the
/// workspace is deleted. A folder that is not there is fine.
///
/// # Errors
///
/// A folder exists and could not be removed (a window still has it open). The other one is still
/// tried.
pub fn delete_data_folder(webview_root: &Path, workspace: &WorkspaceName) -> std::io::Result<()> {
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
    pub workspace: WorkspaceName,
    /// The editor's origin, `http://<workspace>.localhost:<port>/` (the front's `origin`).
    pub origin: Url,
    /// The webview's data folder ([`data_folder`]).
    pub data_folder: PathBuf,
    /// The workspace's window settings.
    pub settings: WindowSettings,
    /// The editor's session cookie, when its server asks for a password ([`signin::sign_in`]).
    pub cookie: Option<SessionCookie>,
    /// A port for WebView2's DevTools Protocol, for the smoke test (Windows only; the window is
    /// otherwise built as for a user).
    pub debug_port: Option<u16>,
}

/// What the windows ask of the shell.
#[derive(Clone)]
pub struct Hooks {
    /// Hands a web address to the default browser.
    pub open_external: OpenExternal,
    /// Tells the user a link was refused.
    pub refused: Refused,
    /// Called when a workspace's last window is gone, so the guest can end the editor's extension
    /// hosts. Called from the window system's thread: it must not wait.
    pub last_closed: Arc<dyn Fn(&WorkspaceName) + Send + Sync>,
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
    open: Mutex<HashMap<WorkspaceName, usize>>,
    /// Source of the popup labels.
    popups: AtomicUsize,
    /// The cookie the last sign-in gave a workspace, with the password it was for: the editor's
    /// login limits how often one can sign in, and a window reopened a moment later must not hit it.
    sessions: Mutex<HashMap<WorkspaceName, (String, SessionCookie)>>,
}

impl SandboxWindows {
    /// A registry with no window open.
    #[must_use]
    pub fn new(hooks: Hooks) -> Self {
        Self {
            inner: Arc::new(Inner {
                hooks,
                open: Mutex::new(HashMap::new()),
                popups: AtomicUsize::new(0),
                sessions: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// How many windows `workspace` has open.
    #[must_use]
    pub fn open_count(&self, workspace: &WorkspaceName) -> usize {
        self.inner
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            .copied()
            .unwrap_or(0)
    }

    fn opened(&self, workspace: &WorkspaceName) {
        *self
            .inner
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(workspace.clone())
            .or_insert(0) += 1;
    }

    /// A window of `workspace` is gone. The hook runs when it was the last one.
    fn closed(&self, workspace: &WorkspaceName) {
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
        if last {
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
        workspace: &WorkspaceName,
        password: &str,
    ) -> Result<SessionCookie, signin::SignInError> {
        if let Some((for_password, cookie)) = self
            .inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(workspace)
            && for_password == password
        {
            return Ok(cookie.clone());
        }
        let cookie = signin::sign_in(front, workspace, password).await?;
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(workspace.clone(), (password.to_owned(), cookie.clone()));
        Ok(cookie)
    }

    /// Forgets what is kept for `workspace` (its session), when the workspace is deleted.
    pub fn forget(&self, workspace: &WorkspaceName) {
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
        let start = if target.cookie.is_some() {
            blank(&target.origin)
        } else {
            target.origin.clone()
        };
        let app = manager.app_handle();
        let window = self.build(app, &target, &label, WebviewUrl::External(start), None)?;
        if let Some(cookie) = &target.cookie {
            if let Err(err) = window.set_cookie(session_cookie(&target, cookie)) {
                // The editor's own login page shows instead; the user can sign in there.
                tracing::warn!(%err, "the editor's session could not be set; its login page shows");
            }
            window.navigate(target.origin.clone())?;
        }
        Ok(window)
    }

    /// The open windows of `workspace`, the first and any further ones.
    fn windows_of<R: Runtime, M: Manager<R>>(
        manager: &M,
        workspace: &WorkspaceName,
    ) -> Vec<WebviewWindow<R>> {
        manager
            .webview_windows()
            .into_iter()
            .filter(|(label, _)| belongs_to(label, workspace))
            .map(|(_, window)| window)
            .collect()
    }

    /// Closes every window of `workspace` the way a user would, so the editor disconnects cleanly.
    /// The close is asked for, not waited for; the last window's end runs the hook.
    pub fn close<R: Runtime, M: Manager<R>>(&self, manager: &M, workspace: &WorkspaceName) {
        for window in Self::windows_of(manager, workspace) {
            // A window that is already going away can't be closed again.
            let _ = window.close();
        }
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
            .title(format!("{} - Puddle", target.workspace))
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
        // Only the editor's windows are counted: the cleanup hook ends the editor's extension hosts
        // when the last of them is gone, whatever apps are still open on forwarded ports.
        let counted = !is_forward(target);
        if counted {
            self.opened(&target.workspace);
        }
        let window = match builder.build() {
            Ok(window) => window,
            Err(err) => {
                if counted {
                    self.closed(&target.workspace);
                }
                return Err(err);
            }
        };
        if counted {
            window.on_window_event(self.on_event(target.workspace.clone()));
        }
        #[cfg(windows)]
        accelerators::turn_off(&window);
        Ok(window)
    }

    /// What a window of `workspace` does with its events: the end of the window is counted.
    fn on_event(&self, workspace: WorkspaceName) -> impl Fn(&WindowEvent) + Send + 'static {
        let this = self.clone();
        move |event| {
            if matches!(event, WindowEvent::Destroyed) {
                this.closed(&workspace);
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
                    Ok(window) => NewWindowResponse::Create { window },
                    Err(err) => {
                        tracing::warn!(%err, "a new editor window could not be built");
                        // The user clicked New Window: say that nothing opened.
                        (hooks.refused)(url);
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
        .http_only(true)
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

#[cfg(windows)]
mod accelerators {
    //! WebView2's own shortcuts (F7's caret-browsing prompt, print, find) off, so the editor gets
    //! every key. The setting has no portable counterpart: WebKitGTK has none to turn off.

    use tauri::{Runtime, WebviewWindow};
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2Settings3;
    use windows::core::Interface;

    /// Turns the browser accelerator keys off in `window`'s webview. A failure is logged: the
    /// window still works, with the browser's shortcuts on.
    pub(super) fn turn_off<R: Runtime>(window: &WebviewWindow<R>) {
        let label = window.label().to_owned();
        let asked = window.with_webview(move |webview| {
            // SAFETY: `controller()` is the live controller of this window's webview, called on
            // the thread that owns it (`with_webview` runs this closure there); the calls only read
            // the settings object it returns and set one boolean on it.
            #[expect(unsafe_code, reason = "WebView2 COM call with no safe wrapper")]
            let done = unsafe {
                webview
                    .controller()
                    .CoreWebView2()
                    .and_then(|core| core.Settings())
                    .and_then(|settings| settings.cast::<ICoreWebView2Settings3>())
                    .and_then(|settings| settings.SetAreBrowserAcceleratorKeysEnabled(false))
            };
            if let Err(err) = done {
                tracing::warn!(%err, label, "the browser shortcuts stay on in an editor window");
            }
        });
        if let Err(err) = asked {
            tracing::warn!(%err, "the browser shortcuts stay on in an editor window");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    fn workspace(name: &str) -> WorkspaceName {
        WorkspaceName::new(name).unwrap()
    }

    fn policy() -> OriginPolicy {
        OriginPolicy::new(&Url::parse("http://demo.localhost:4800/").unwrap(), None)
    }

    #[test]
    fn the_hooks_show_in_debug_output_without_their_closures() {
        let Fixture { windows, .. } = fixture();
        assert!(format!("{windows:?}").contains("Hooks"));
    }

    #[test]
    fn a_workspace_window_has_a_label_the_main_window_never_has() {
        assert_eq!(label(&workspace("demo")), "vs-demo");
        assert!(is_sandbox_label("vs-demo"));
        assert!(is_sandbox_label("vs-demo:3"));
        assert!(!is_sandbox_label(crate::window::MAIN_LABEL));
        assert!(remembers_place(crate::window::MAIN_LABEL));
        assert!(!remembers_place("vs-demo"));
    }

    #[test]
    fn a_further_window_has_a_label_no_workspace_can_have() {
        // `vs-demo-1` is the first window of the workspace `demo-1`; the further windows of `demo`
        // use a colon, which a workspace name cannot hold.
        let demo = workspace("demo");
        assert_eq!(popup_label(&demo, 1), "vs-demo:1");
        assert!(belongs_to("vs-demo", &demo));
        assert!(belongs_to("vs-demo:1", &demo));
        assert!(!belongs_to("vs-demo-1", &demo));
        assert!(!belongs_to("vs-demo2", &demo));
        assert!(!belongs_to("main", &demo));
        assert!(WorkspaceName::new("demo:1").is_err());
    }

    #[test]
    fn each_workspace_has_a_data_folder_of_its_own_that_goes_with_it() {
        let root = tempfile::tempdir().unwrap();
        let (a, b) = (workspace("alpha"), workspace("beta"));
        assert_ne!(data_folder(root.path(), &a), data_folder(root.path(), &b));
        std::fs::create_dir_all(data_folder(root.path(), &a).join("EBWebView")).unwrap();
        std::fs::create_dir_all(data_folder(root.path(), &b)).unwrap();
        // The windows on forwarded ports keep their data beside the editor's, and go with it.
        let apps = forward_data_folder(&data_folder(root.path(), &a));
        std::fs::create_dir_all(apps.join("EBWebView")).unwrap();
        let others = forward_data_folder(&data_folder(root.path(), &b));
        std::fs::create_dir_all(&others).unwrap();
        delete_data_folder(root.path(), &a).unwrap();
        assert!(!data_folder(root.path(), &a).exists());
        assert!(!apps.exists());
        assert!(data_folder(root.path(), &b).exists());
        assert!(others.exists());
        // Already gone is fine.
        delete_data_folder(root.path(), &a).unwrap();
    }

    #[test]
    fn a_data_folder_that_cannot_be_removed_is_an_error() {
        let root = tempfile::tempdir().unwrap();
        let w = workspace("alpha");
        // A file where the folder should be: `remove_dir_all` says it is not a directory.
        std::fs::write(data_folder(root.path(), &w), b"x").unwrap();
        assert!(delete_data_folder(root.path(), &w).is_err());
    }

    #[test]
    fn the_editor_s_own_pages_become_windows_and_nothing_else_does() {
        let p = policy();
        let class = |u: &str| classify_new_window(&p, &Url::parse(u).unwrap());
        // VS Code's "New Window".
        assert_eq!(
            class("http://demo.localhost:4800/?folder=/work"),
            NewWindow::Create
        );
        // The guest's port proxy: Puddle's own forward, never the webview's pop-up.
        let forward = |u: &str| NewWindow::Forward(Url::parse(u).unwrap());
        assert_eq!(
            class("http://demo.localhost:4800/proxy/5173/"),
            forward("http://5173.demo.localhost:4800/")
        );
        assert_eq!(
            class("http://demo.localhost:4800/absproxy/5173/x"),
            forward("http://5173.demo.localhost:4800/absproxy/5173/x")
        );
        // A link that names no port stays what it was: the default browser.
        assert_eq!(
            class("http://demo.localhost:4800/proxy/web/"),
            NewWindow::External
        );
        // Another origin, even another workspace's.
        assert_eq!(
            class("https://github.com/login/device"),
            NewWindow::External
        );
        assert_eq!(class("http://other.localhost:4800/"), NewWindow::External);
        assert_eq!(class("http://demo.localhost:4801/"), NewWindow::External);
        // Things that are not an address to open.
        for refused in [
            "about:blank",
            "blob:http://demo.localhost:4800/1f2e",
            "http://tauri.localhost/index.html",
            "file:///c:/windows/win.ini",
            "javascript:alert(1)",
            "mailto:a@example.org",
        ] {
            assert_eq!(class(refused), NewWindow::Refuse, "{refused}");
        }
    }

    #[test]
    fn an_app_window_on_a_forwarded_port_does_not_decide_when_the_editor_is_done() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let Fixture { windows, ended } = fixture();
        let t = mock_target("demo", data.path());
        // The editor's window and an app's window; the app's is not counted.
        windows.open(&app, &t).unwrap();
        windows.new_window(
            app.handle(),
            &t,
            &Url::parse("http://demo.localhost:4800/proxy/5173/").unwrap(),
            None,
        );
        assert_eq!(app.webview_windows().len(), 2);
        assert_eq!(windows.open_count(&t.workspace), 1);
        // The editor's window ends: the cleanup runs although the app's window is still open.
        windows.on_event(t.workspace.clone())(&WindowEvent::Destroyed);
        assert_eq!(*ended.lock().unwrap(), ["demo"]);
        // A window the app itself opens on its own origin is an app window too.
        let forwarded =
            forward_target(&t, &Url::parse("http://5173.demo.localhost:4800/").unwrap());
        assert!(is_forward(&forwarded));
        assert!(!is_forward(&t));
    }

    #[test]
    fn a_window_on_a_forwarded_port_is_the_guests_own_app_with_nothing_of_the_editors() {
        let editor = Target {
            cookie: Some(SessionCookie::new("code-server-session", "abc")),
            settings: WindowSettings {
                zoom_hotkeys: false,
                clipboard_read: ClipboardRead::Allow,
            },
            ..target("demo", Path::new("/data"), None)
        };
        let address = Url::parse("http://5173.demo.localhost:4800/a/b?c=1#d").unwrap();
        let forwarded = forward_target(&editor, &address);
        assert_eq!(
            forwarded.origin.as_str(),
            "http://5173.demo.localhost:4800/"
        );
        assert_eq!(forwarded.workspace, editor.workspace);
        assert!(
            forwarded.cookie.is_none(),
            "the editor's sign-in stays with the editor"
        );
        // A page of the guest's does not read the clipboard unasked, whatever the editor may.
        assert_eq!(forwarded.settings.clipboard_read, ClipboardRead::Ask);
        assert!(!forwarded.settings.zoom_hotkeys);
        for (set, expected) in [
            (ClipboardRead::Ask, ClipboardRead::Ask),
            (ClipboardRead::Deny, ClipboardRead::Deny),
        ] {
            let mut again = editor.clone();
            again.settings.clipboard_read = set;
            assert_eq!(
                forward_target(&again, &address).settings.clipboard_read,
                expected
            );
        }
        assert_ne!(forwarded.data_folder, editor.data_folder);
        assert_eq!(forwarded.data_folder.file_name().unwrap(), "demo.forward");
        assert_eq!(forwarded.data_folder.parent(), editor.data_folder.parent());
        assert_eq!(forwarded.debug_port, None);
        let probed = Target {
            debug_port: Some(9334),
            ..editor.clone()
        };
        assert_eq!(
            forward_target(&probed, &address).debug_port,
            Some(9335),
            "the app's browser process gets a DevTools port of its own"
        );
    }

    #[test]
    fn a_forward_address_keeps_the_rest_of_the_link_and_only_a_real_port_gets_one() {
        let url = |u: &str| Url::parse(u).unwrap();
        let rewrite = |u: &str| forward_url(&url(u)).map(String::from);
        assert_eq!(
            rewrite("http://demo.localhost:4800/proxy/5173/a%20b/c?x=1&y=%2F#frag").as_deref(),
            Some("http://5173.demo.localhost:4800/a%20b/c?x=1&y=%2F#frag")
        );
        // No slash after the port: the app's root.
        assert_eq!(
            rewrite("http://demo.localhost:4800/proxy/80").as_deref(),
            Some("http://80.demo.localhost:4800/")
        );
        assert_eq!(
            rewrite("http://demo.localhost:4800/absproxy/65535").as_deref(),
            Some("http://65535.demo.localhost:4800/absproxy/65535")
        );
        for bad in [
            "/proxy/",
            "/proxy/0/",
            "/proxy/00080/",
            "/proxy/65536/",
            "/proxy/-1/",
            "/proxy/+80/",
            "/proxy/80a/",
            "/proxy/%38%30/",
            "/absproxy/",
            "/other/80/",
            "/",
        ] {
            assert_eq!(
                rewrite(&format!("http://demo.localhost:4800{bad}")),
                None,
                "{bad}"
            );
        }
    }

    #[test]
    fn on_a_forwarded_app_s_origin_proxy_paths_are_the_app_s_own() {
        let policy = OriginPolicy::new(
            &Url::parse("http://5173.demo.localhost:4800/").unwrap(),
            None,
        );
        let class = |u: &str| classify(&policy, &Url::parse(u).unwrap(), false);
        assert_eq!(
            class("http://5173.demo.localhost:4800/proxy/80/"),
            NewWindow::Create
        );
        // The editor's origin is another origin from here, and so is another port's.
        assert_eq!(
            class("http://demo.localhost:4800/proxy/80/"),
            NewWindow::External
        );
        assert_eq!(class("http://80.demo.localhost:4800/"), NewWindow::External);
    }

    #[test]
    fn a_script_s_clipboard_read_follows_the_setting_and_nothing_else_is_decided() {
        let with = |clipboard_read| WindowSettings {
            zoom_hotkeys: true,
            clipboard_read,
        };
        let clip = PermissionKind::ClipboardRead;
        assert_eq!(
            permission_response(&with(ClipboardRead::Allow), &clip),
            PermissionResponse::Allow
        );
        assert_eq!(
            permission_response(&with(ClipboardRead::Deny), &clip),
            PermissionResponse::Deny
        );
        // Ask: the webview's own prompt, which keeps the answer per data folder.
        assert_eq!(
            permission_response(&with(ClipboardRead::Ask), &clip),
            PermissionResponse::Default
        );
        for setting in [
            ClipboardRead::Allow,
            ClipboardRead::Deny,
            ClipboardRead::Ask,
        ] {
            for other in [PermissionKind::Geolocation, PermissionKind::Notifications] {
                assert_eq!(
                    permission_response(&with(setting), &other),
                    PermissionResponse::Default,
                    "{other:?} with {setting:?}"
                );
            }
        }
    }

    #[test]
    fn the_settings_come_from_the_workspace_s_effective_values() {
        let mut global = puddle_settings::GlobalSettings::default();
        global.workspace_defaults.zoom_hotkeys = Some(false);
        global.workspace_defaults.clipboard_read = Some(ClipboardRead::Deny);
        let effective = puddle_settings::resolve(&global, None);
        assert_eq!(
            WindowSettings::from(&effective),
            WindowSettings {
                zoom_hotkeys: false,
                clipboard_read: ClipboardRead::Deny
            }
        );
        let defaults = puddle_settings::resolve(&puddle_settings::GlobalSettings::default(), None);
        assert_eq!(
            WindowSettings::from(&defaults),
            WindowSettings {
                zoom_hotkeys: true,
                clipboard_read: ClipboardRead::Ask
            }
        );
    }

    type Sink = Arc<dyn Fn(&Url) + Send + Sync>;

    fn recorder() -> (Sink, Arc<Mutex<Vec<String>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (
            Arc::new(move |u: &Url| sink.lock().unwrap().push(u.to_string())),
            seen,
        )
    }

    struct Fixture {
        windows: SandboxWindows,
        ended: Arc<Mutex<Vec<String>>>,
    }

    fn fixture() -> Fixture {
        let (open_external, _) = recorder();
        let (refused, _) = recorder();
        let ended = Arc::new(Mutex::new(Vec::new()));
        let sink = ended.clone();
        let windows = SandboxWindows::new(Hooks {
            open_external,
            refused,
            last_closed: Arc::new(move |w| sink.lock().unwrap().push(w.to_string())),
        });
        Fixture { windows, ended }
    }

    #[test]
    fn the_cleanup_hook_runs_when_the_last_window_of_a_workspace_is_gone() {
        let Fixture { windows, ended } = fixture();
        let (a, b) = (workspace("alpha"), workspace("beta"));
        windows.opened(&a);
        windows.opened(&a);
        windows.opened(&b);
        assert_eq!((windows.open_count(&a), windows.open_count(&b)), (2, 1));
        windows.closed(&a);
        assert!(ended.lock().unwrap().is_empty(), "alpha still has a window");
        windows.closed(&a);
        assert_eq!(*ended.lock().unwrap(), ["alpha"]);
        assert_eq!(windows.open_count(&a), 0);
        // A window that was never counted does not end anything.
        windows.closed(&a);
        assert_eq!(*ended.lock().unwrap(), ["alpha"]);
        windows.closed(&b);
        assert_eq!(*ended.lock().unwrap(), ["alpha", "beta"]);
    }

    fn target(name: &str, data: &Path, cookie: Option<SessionCookie>) -> Target {
        Target {
            workspace: workspace(name),
            origin: Url::parse(&format!("http://{name}.localhost:4800/")).unwrap(),
            data_folder: data.join(name),
            settings: WindowSettings {
                zoom_hotkeys: true,
                clipboard_read: ClipboardRead::Ask,
            },
            cookie,
            debug_port: None,
        }
    }

    #[test]
    fn a_window_opens_on_the_editor_s_origin_with_its_data_folder_and_is_counted() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let Fixture { windows, .. } = fixture();
        let t = target("demo", data.path(), None);
        let window = windows.open(&app, &t).unwrap();
        assert_eq!(window.label(), "vs-demo");
        assert_eq!(window.url().unwrap(), t.origin);
        assert!(t.data_folder.is_dir());
        assert_eq!(windows.open_count(&t.workspace), 1);
        // Open again: the same window, no second one, no second count.
        let again = windows.open(&app, &t).unwrap();
        assert_eq!(again.label(), window.label());
        assert_eq!(app.webview_windows().len(), 1);
        assert_eq!(windows.open_count(&t.workspace), 1);
    }

    #[test]
    fn with_a_session_the_window_goes_to_the_editor_after_the_cookie_is_set() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let Fixture { windows, .. } = fixture();
        let cookie = SessionCookie::new("code-server-session", "abc");
        let t = target("demo", data.path(), Some(cookie.clone()));
        let window = windows.open(&app, &t).unwrap();
        assert_eq!(window.url().unwrap(), t.origin);
        let set = session_cookie(&t, &cookie);
        assert_eq!(
            (
                set.name(),
                set.value(),
                set.domain(),
                set.path(),
                set.http_only()
            ),
            (
                "code-server-session",
                "abc",
                Some("demo.localhost"),
                Some("/"),
                Some(true)
            )
        );
        // The credential is never part of the address.
        assert!(!window.url().unwrap().as_str().contains("abc"));
    }

    #[test]
    fn closing_reaches_every_window_of_the_workspace_and_no_other() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let Fixture { windows, .. } = fixture();
        let (a, b) = (
            target("alpha", data.path(), None),
            target("beta", data.path(), None),
        );
        windows.open(&app, &a).unwrap();
        windows.open(&app, &b).unwrap();
        let mut labels: Vec<_> = SandboxWindows::windows_of(&app, &a.workspace)
            .iter()
            .map(|w| w.label().to_owned())
            .collect();
        labels.sort();
        assert_eq!(labels, ["vs-alpha"]);
        assert_eq!(
            SandboxWindows::windows_of(&app, &workspace("nothing-open")).len(),
            0
        );
        // The mock window system never acts on a close request, so only that it is asked shows here.
        windows.close(&app, &a.workspace);
        windows.close(&app, &workspace("nothing-open"));
    }

    fn mock_target(name: &str, data: &Path) -> Arc<Target> {
        Arc::new(target(name, data, None))
    }

    #[test]
    fn a_new_window_request_creates_a_guarded_window_or_goes_to_the_browser_or_is_dropped() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let (open_external, opened) = recorder();
        let (refused, refusals) = recorder();
        let windows = SandboxWindows::new(Hooks {
            open_external,
            refused,
            last_closed: Arc::new(|_| {}),
        });
        let t = mock_target("demo", data.path());
        let ask = |url: &str| windows.new_window(app.handle(), &t, &Url::parse(url).unwrap(), None);
        // VS Code's "New Window": a window of its own label, counted with the first.
        assert!(matches!(
            ask("http://demo.localhost:4800/?folder=/work"),
            NewWindowResponse::Create { .. }
        ));
        assert!(app.get_webview_window("vs-demo:0").is_some());
        assert!(matches!(
            ask("http://demo.localhost:4800/"),
            NewWindowResponse::Create { .. }
        ));
        assert!(app.get_webview_window("vs-demo:1").is_some());
        assert_eq!(windows.open_count(&t.workspace), 2);
        // Anything else never becomes a window, and never `Allow`.
        assert!(matches!(
            ask("https://example.org/a"),
            NewWindowResponse::Deny
        ));
        assert!(matches!(
            ask("mailto:a@example.org"),
            NewWindowResponse::Deny
        ));
        assert_eq!(*opened.lock().unwrap(), ["https://example.org/a"]);
        assert_eq!(*refusals.lock().unwrap(), ["mailto:a@example.org"]);
        assert_eq!(app.webview_windows().len(), 2);
    }

    #[test]
    fn a_proxy_link_opens_a_guarded_window_on_the_forwarded_port_s_own_origin() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let (open_external, opened) = recorder();
        let (refused, refusals) = recorder();
        let windows = SandboxWindows::new(Hooks {
            open_external,
            refused,
            last_closed: Arc::new(|_| {}),
        });
        let t = mock_target("demo", data.path());
        let ask = |url: &str| windows.new_window(app.handle(), &t, &Url::parse(url).unwrap(), None);
        // The editor's `/proxy/<port>/` link: a window of ours on `<port>.<workspace>.localhost`,
        // with the path after the port; the default browser hears nothing.
        assert!(matches!(
            ask("http://demo.localhost:4800/proxy/5173/app/?a=1#top"),
            NewWindowResponse::Deny
        ));
        let window = app
            .get_webview_window("vs-demo:0")
            .expect("a window opened");
        assert_eq!(
            window.url().unwrap().as_str(),
            "http://5173.demo.localhost:4800/app/?a=1#top"
        );
        // Not an editor window: it does not hold up the editor's cleanup (see the next test).
        assert_eq!(windows.open_count(&t.workspace), 0);
        // Its data is not the editor's: the page there is the guest's own app.
        assert!(data.path().join("demo.forward").is_dir());
        assert!(!t.data_folder.exists());
        // `/absproxy/` keeps its path: the app is told to serve under it.
        ask("http://demo.localhost:4800/absproxy/3000/x");
        assert_eq!(
            app.get_webview_window("vs-demo:1")
                .unwrap()
                .url()
                .unwrap()
                .as_str(),
            "http://3000.demo.localhost:4800/absproxy/3000/x"
        );
        // Not a port: the old answer, the default browser.
        for bad in [
            "proxy/0/",
            "proxy/65536/",
            "proxy/abc/",
            "proxy/",
            "proxy/05173/",
        ] {
            ask(&format!("http://demo.localhost:4800/{bad}"));
        }
        assert_eq!(opened.lock().unwrap().len(), 5);
        assert!(refusals.lock().unwrap().is_empty());
        assert_eq!(app.webview_windows().len(), 2);
    }

    #[test]
    fn a_new_window_that_cannot_be_built_says_so_and_is_not_counted() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let (open_external, _) = recorder();
        let (refused, refusals) = recorder();
        let windows = SandboxWindows::new(Hooks {
            open_external,
            refused,
            last_closed: Arc::new(|_| {}),
        });
        let t = mock_target("demo", data.path());
        // The label it would take is already in use.
        windows
            .build(
                app.handle(),
                &t,
                "vs-demo:0",
                WebviewUrl::External(t.origin.clone()),
                None,
            )
            .unwrap();
        assert_eq!(windows.open_count(&t.workspace), 1);
        let url = Url::parse("http://demo.localhost:4800/").unwrap();
        let answer = windows.new_window(app.handle(), &t, &url, None);
        assert!(matches!(answer, NewWindowResponse::Deny));
        assert_eq!(*refusals.lock().unwrap(), [url.to_string()]);
        assert_eq!(windows.open_count(&t.workspace), 1);
    }

    #[test]
    fn an_app_window_that_cannot_be_built_says_so_and_opens_nothing_in_the_browser() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let (open_external, opened) = recorder();
        let (refused, refusals) = recorder();
        let windows = SandboxWindows::new(Hooks {
            open_external,
            refused,
            last_closed: Arc::new(|_| {}),
        });
        let t = mock_target("demo", data.path());
        // The label the window would take is already in use.
        windows
            .build(
                app.handle(),
                &t,
                "vs-demo:0",
                WebviewUrl::External(t.origin.clone()),
                None,
            )
            .unwrap();
        let link = Url::parse("http://demo.localhost:4800/proxy/5173/").unwrap();
        assert!(matches!(
            windows.new_window(app.handle(), &t, &link, None),
            NewWindowResponse::Deny
        ));
        assert_eq!(*refusals.lock().unwrap(), [link.to_string()]);
        assert!(opened.lock().unwrap().is_empty());
        assert_eq!(app.webview_windows().len(), 1);
    }

    #[test]
    fn a_window_s_end_is_counted_and_nothing_else_is() {
        let Fixture { windows, ended } = fixture();
        let w = workspace("demo");
        windows.opened(&w);
        let on_event = windows.on_event(w.clone());
        on_event(&WindowEvent::Focused(true));
        assert_eq!(windows.open_count(&w), 1);
        on_event(&WindowEvent::Destroyed);
        assert_eq!(windows.open_count(&w), 0);
        assert_eq!(*ended.lock().unwrap(), ["demo"]);
    }

    #[test]
    fn a_window_asks_the_settings_for_its_permissions() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let Fixture { windows, .. } = fixture();
        let t = target("demo", data.path(), None);
        let window = windows.open(&app, &t).unwrap();
        let allow = permission_handler::<tauri::test::MockRuntime>(WindowSettings {
            zoom_hotkeys: true,
            clipboard_read: ClipboardRead::Allow,
        });
        assert_eq!(
            allow(window.as_ref().clone(), PermissionKind::ClipboardRead),
            PermissionResponse::Allow
        );
        assert_eq!(
            allow(window.as_ref().clone(), PermissionKind::Geolocation),
            PermissionResponse::Default
        );
    }

    #[test]
    fn a_window_that_cannot_be_built_is_not_left_counted() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let Fixture { windows, .. } = fixture();
        let t = mock_target("demo", data.path());
        let start = || WebviewUrl::External(t.origin.clone());
        windows
            .build(app.handle(), &t, "vs-demo", start(), None)
            .unwrap();
        assert!(
            windows
                .build(app.handle(), &t, "vs-demo", start(), None)
                .is_err()
        );
        assert_eq!(windows.open_count(&t.workspace), 1);
    }

    #[test]
    fn a_data_folder_that_cannot_be_made_stops_the_open() {
        let app = tauri::test::mock_app();
        let data = tempfile::tempdir().unwrap();
        let Fixture { windows, .. } = fixture();
        let mut t = target("demo", data.path(), None);
        // A file where the folder should be.
        std::fs::write(&t.data_folder, b"x").unwrap();
        t.data_folder = t.data_folder.join("inside");
        assert!(windows.open(&app, &t).is_err());
        assert_eq!(app.webview_windows().len(), 0);
        assert_eq!(windows.open_count(&t.workspace), 0);
    }

    /// A one-answer-per-connection editor that counts the logins it got.
    async fn counting_editor() -> (std::net::SocketAddr, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                seen.fetch_add(1, Ordering::SeqCst);
                let mut buffer = vec![0; 4096];
                let _ = stream.read(&mut buffer).await;
                stream
                    .write_all(
                        b"HTTP/1.1 302 Found\r\nSet-Cookie: code-server-session=abc; Path=/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
            }
        });
        (addr, count)
    }

    #[tokio::test]
    async fn a_session_is_reused_for_the_same_password_and_renewed_for_another() {
        let Fixture { windows, .. } = fixture();
        let (addr, logins) = counting_editor().await;
        let demo = workspace("demo");
        let first = windows.session(addr, &demo, "one").await.unwrap();
        let second = windows.session(addr, &demo, "one").await.unwrap();
        assert_eq!(first, second);
        assert_eq!(logins.load(Ordering::SeqCst), 1);
        windows.session(addr, &demo, "two").await.unwrap();
        assert_eq!(logins.load(Ordering::SeqCst), 2);
        windows.forget(&demo);
        windows.session(addr, &demo, "two").await.unwrap();
        assert_eq!(logins.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_session_that_cannot_be_had_is_an_error_and_is_not_kept() {
        let Fixture { windows, .. } = fixture();
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = free.local_addr().unwrap();
        drop(free);
        assert!(
            windows
                .session(addr, &workspace("demo"), "x")
                .await
                .is_err()
        );
    }
}
