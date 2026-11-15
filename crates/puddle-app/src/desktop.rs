// SPDX-License-Identifier: AGPL-3.0-or-later
//! The desktop behaviour around the main window: what the close and minimize buttons do, the tray,
//! the badge, and the operating-system notifications. It runs in Rust, driven by the backend's
//! event hub and the settings; the page calls no Tauri API for any of it.
//!
//! Settings are read where they are used (the close button reads them when it is pressed, the
//! event loop on every event and every few seconds), so a changed switch applies without a restart.

use std::fs::File;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use puddle_api::SettingsRepo;
use puddle_settings::{GlobalSettings, UiPrefs};
use puddle_store::{Store, UserId};
use puddle_types::Event;
use serde_json::Value;
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Manager, Runtime, WebviewWindow, WindowEvent};
use tauri_plugin_notification::{ActionPerformed, NotificationExt};
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use url::Url;

use crate::autostart::{self, LoginEntry};
use crate::backend::{Backend, WorkspaceLabel};
use crate::badge::{self, OpenRequests};
use crate::icon::{self, Wanted};
use crate::notify::{self, Notice, Notifier, REQUEST_EXTRA};
use crate::quit_shortcut;
use crate::tray::{self, CloseAction, MENU_QUIT, MinimizeAction, Tray, TrayState};
use crate::window::MAIN_LABEL;

/// How often the settings are read again while no event arrives, so a switch changed in Settings
/// shows on the badge soon.
const SETTINGS_POLL: Duration = Duration::from_secs(3);

/// The marker file that says the "Puddle is still running" notice was shown.
const NOTICE_MARKER: &str = "tray-notice-shown";

/// The window preferences in `repo`.
///
/// # Errors
///
/// What is wrong with the stored settings, in words; the caller falls back to the defaults.
pub fn read_prefs(repo: &dyn SettingsRepo) -> Result<UiPrefs, String> {
    let Some(document) = repo
        .load_global()
        .map_err(|e| format!("the settings cannot be read: {e}"))?
    else {
        return Ok(UiPrefs::default());
    };
    GlobalSettings::from_document(document)
        .map(|loaded| loaded.settings.ui)
        .map_err(|e| format!("the settings are not valid: {e}"))
}

/// The preferences, or the defaults (with a warning) when the settings cannot be read: the window
/// must still close.
fn prefs_or_default(repo: &dyn SettingsRepo) -> UiPrefs {
    read_prefs(repo).unwrap_or_else(|reason| {
        tracing::warn!(%reason, "using the default window preferences");
        UiPrefs::default()
    })
}

/// Where a click on a notification goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opens {
    /// The inbox as it is.
    Inbox,
    /// The inbox with this request current.
    Request(i64),
}

/// The address a notification opens.
#[must_use]
pub fn inbox_url(base: &Url, opens: Opens) -> Url {
    let mut url = base.clone();
    url.set_path("/inbox");
    match opens {
        Opens::Inbox => url.set_query(None),
        Opens::Request(id) => url.set_query(Some(&format!("request={id}"))),
    }
    url
}

/// What a click on a notification asks for: `None` for any action but the click itself.
fn tapped(performed: &ActionPerformed) -> Option<Opens> {
    (performed.action_id() == "tap").then(|| {
        performed
            .notification()
            .and_then(|n| n.extra().get(REQUEST_EXTRA))
            .and_then(Value::as_i64)
            .map_or(Opens::Inbox, Opens::Request)
    })
}

/// A click on a notification: opens the inbox where the notification pointed.
fn on_tap<R: Runtime>(app: &AppHandle<R>, base: &Url, performed: &ActionPerformed) {
    if let Some(opens) = tapped(performed) {
        open_inbox(app, base, opens);
    }
}

/// Brings the main window up on the inbox, at the request a notification was about.
fn open_inbox<R: Runtime>(app: &AppHandle<R>, base: &Url, opens: Opens) {
    crate::shell::focus_main(app);
    if let Some(window) = app.get_webview_window(MAIN_LABEL)
        && let Err(err) = window.navigate(inbox_url(base, opens))
    {
        tracing::warn!(%err, "could not open the inbox from a notification");
    }
}

/// Whether Puddle's window is the one in front: shown, not minimized and focused.
fn in_front<R: Runtime>(app: &AppHandle<R>) -> bool {
    app.get_webview_window(MAIN_LABEL).is_some_and(|w| {
        w.is_visible().unwrap_or(false)
            && !w.is_minimized().unwrap_or(false)
            && w.is_focused().unwrap_or(false)
    })
}

/// The ids of the requests that wait now.
fn open_requests(store: &Store) -> OpenRequests {
    match store.open_pending(UserId::OWNER, None) {
        Ok(rows) => OpenRequests::of(rows.into_iter().map(|r| r.id.0)),
        Err(err) => {
            tracing::warn!(%err, "could not count the waiting requests");
            OpenRequests::default()
        }
    }
}

/// True the first time, then false: with a marker file in the data folder it holds across runs,
/// without one (no folder to write to) it holds for this run.
fn first_time(marker: Option<&Path>, this_run: &AtomicBool) -> bool {
    if let Some(path) = marker {
        match File::create_new(path.join(NOTICE_MARKER)) {
            Ok(_) => return true,
            Err(err) if err.kind() == ErrorKind::AlreadyExists => return false,
            Err(err) => tracing::debug!(%err, "no marker file; the notice shows once per run"),
        }
    }
    !this_run.swap(true, Ordering::SeqCst)
}

/// What the main window's buttons need to decide.
struct Buttons {
    settings: Arc<dyn SettingsRepo>,
    tray: TrayState,
    marker: Option<PathBuf>,
    told: AtomicBool,
    /// Shows the once-only notice that Puddle went to the tray.
    tell: Arc<dyn Fn(&Notice) + Send + Sync>,
}

impl Buttons {
    /// The close button was pressed. Returns whether the window must stay (`true`) or close.
    fn on_close<R: Runtime>(&self, window: &WebviewWindow<R>) -> bool {
        let prefs = prefs_or_default(self.settings.as_ref());
        match tray::close_action(&prefs, self.tray) {
            // The window closes, the last one ends the app and the exit hook runs the quit.
            CloseAction::Quit => false,
            CloseAction::HideToTray => {
                let _ = window.hide();
                tracing::info!("the close button hid the window in the tray");
                if first_time(self.marker.as_deref(), &self.told) {
                    (self.tell)(&Notice {
                        title: "Puddle is still running".to_owned(),
                        body: "It stays in the tray so your workspaces keep working. Quit from \
                               the tray menu stops them."
                            .to_owned(),
                        request: None,
                    });
                }
                true
            }
            CloseAction::Minimize => {
                // a window the system refuses to minimize stays visible, which the log line below already tells
                let _ = window.minimize();
                tracing::info!("there is no tray icon, so the close button minimized the window");
                true
            }
        }
    }

    /// The window was minimized: hide it too when Settings say minimizing goes to the tray.
    /// Returns whether it was hidden.
    fn on_minimize<R: Runtime>(&self, window: &WebviewWindow<R>) -> bool {
        let prefs = prefs_or_default(self.settings.as_ref());
        let hide = tray::minimize_action(&prefs, self.tray) == MinimizeAction::HideToTray;
        if hide {
            // a window that cannot hide stays visible, which is harmless
            let _ = window.hide();
        }
        hide
    }
}

/// The close and minimize buttons of the main window.
fn on_window_event<R: Runtime>(window: &WebviewWindow<R>, event: &WindowEvent, buttons: &Buttons) {
    match event {
        WindowEvent::CloseRequested { api, .. } => {
            if buttons.on_close(window) {
                api.prevent_close();
            }
        }
        // There is no minimize event: a minimized window is a resized one, and the settings are
        // only read for that case.
        WindowEvent::Resized(_) if window.is_minimized().unwrap_or(false) => {
            buttons.on_minimize(window);
        }
        _ => {}
    }
}

/// An application menu whose only entry is Quit with Ctrl+Q (Cmd+Q on macOS), and the main window
/// without its menu bar: the entry is there for its shortcut. Needs the real window system (the
/// test runtime has no menus).
///
/// # Errors
///
/// Whatever Tauri reports when the menu can't be made.
pub fn install_quit_shortcut<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, MENU_QUIT, "Quit Puddle", true, Some("CmdOrCtrl+Q"))?;
    let menu = Menu::with_items(app, &[&Submenu::with_items(app, "Puddle", true, &[&quit])?])?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, event| {
        if event.id().as_ref() == MENU_QUIT {
            quit_shortcut::pressed(app);
        }
    });
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        window.hide_menu()?;
    }
    Ok(())
}

/// What the quit question says, or `None` when no terminal is open and the shortcut just quits.
fn quit_question(open_terminals: usize) -> Option<String> {
    let ends = match open_terminals {
        0 => return None,
        1 => "1 open terminal".to_owned(),
        n => format!("{n} open terminals"),
    };
    Some(format!(
        "Quit Puddle? This stops every workspace and ends {ends}."
    ))
}

/// Answers the question: `true` quits.
pub(crate) type QuitAnswer = Box<dyn FnOnce(bool) + Send>;

/// Ctrl+Q was pressed. With no terminal open it quits at once; otherwise it hands the question to
/// `ask`, quits when the answer is yes and ignores the shortcut while a question is still up.
pub(crate) fn on_quit_pressed(
    open_terminals: usize,
    asking: &Arc<AtomicBool>,
    ask: impl FnOnce(String, QuitAnswer),
    quit: impl FnOnce() + Send + 'static,
) {
    let Some(question) = quit_question(open_terminals) else {
        quit();
        return;
    };
    if asking.swap(true, Ordering::SeqCst) {
        return;
    }
    let asking = Arc::clone(asking);
    ask(
        question,
        Box::new(move |yes| {
            asking.store(false, Ordering::SeqCst);
            if yes {
                quit();
            }
        }),
    );
}

/// Shows a notification, with the system sound when the flag is set.
type ShowNotice = Arc<dyn Fn(&Notice, bool) + Send + Sync>;

/// Where the loop's results go: the operating system in the app, a recorder in tests.
#[derive(Clone)]
struct Outputs {
    /// Whether Puddle's window is the one in front.
    in_front: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Shows a notification, with the system sound when the flag is set.
    notify: ShowNotice,
    /// Shows the waiting count (and whether the badge is on) on the title, the tray and the taskbar.
    badge: Arc<dyn Fn(usize, bool) + Send + Sync>,
    /// Sets the app icon of the window, the taskbar and the tray.
    icon: Arc<dyn Fn(&puddle_appicon::Icon) + Send + Sync>,
    /// Makes the system's start-at-sign-in entry match the Settings switch.
    login: Arc<dyn Fn(bool) + Send + Sync>,
}

impl Outputs {
    /// The real outputs: the main window, the notification plugin and `tray`.
    fn system<R: Runtime>(
        app: &AppHandle<R>,
        tray: Option<Arc<Tray<R>>>,
        login: Option<Arc<dyn LoginEntry>>,
    ) -> Self {
        let front = app.clone();
        let notifying = app.clone();
        let showing = app.clone();
        let decorated = app.clone();
        let tray_icon = tray.clone();
        Self {
            in_front: Arc::new(move || in_front(&front)),
            notify: Arc::new(move |notice, sound| notify::show(&notifying, notice, sound)),
            badge: Arc::new(move |count, on| show_badge(&showing, tray.as_deref(), count, on)),
            icon: Arc::new(move |picture| {
                show_icon(&decorated, tray_icon.as_deref(), &icon::image(picture));
            }),
            login: Arc::new(move |on| {
                if let Some(entry) = &login {
                    set_login(entry.as_ref(), on, std::env::current_exe());
                }
            }),
        }
    }
}

/// Makes `entry` match the switch for the program at `exe`. A failure is logged, never fatal: the
/// switch is kept in the settings, and the next start tries again.
fn set_login(entry: &dyn LoginEntry, on: bool, exe: std::io::Result<PathBuf>) {
    let applied = exe
        .map_err(|err| format!("the program's own path is unknown: {err}"))
        .and_then(|exe| autostart::apply(entry, on, &exe).map_err(|err| err.to_string()));
    match applied {
        Ok(change) => tracing::info!(?change, on, "the start-at-sign-in entry is as wanted"),
        Err(reason) => tracing::warn!(on, %reason, "the start-at-sign-in entry was not changed"),
    }
}

/// What a start at sign-in does with the main window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoginStart {
    /// Hide it: the tray icon is there to open it from.
    HideToTray,
    /// Minimize it: with no tray icon there is nothing to open a hidden window from.
    Minimize,
}

fn login_start(tray: TrayState) -> LoginStart {
    match tray {
        TrayState::Shown => LoginStart::HideToTray,
        TrayState::Missing => LoginStart::Minimize,
    }
}

/// Puts the main window out of the way for a start at sign-in.
pub fn start_out_of_the_way<R: Runtime>(window: &WebviewWindow<R>, tray: TrayState) {
    let done = match login_start(tray) {
        LoginStart::HideToTray => window.hide(),
        LoginStart::Minimize => window.minimize(),
    };
    if let Err(err) = done {
        tracing::warn!(%err, "the window did not step aside for a start at sign-in");
    }
}

/// Sets `picture` as the icon of the main window (the taskbar button and the title bar) and of the
/// tray. A platform with no window icon (macOS) ignores the window's.
fn show_icon<R: Runtime>(
    app: &AppHandle<R>,
    tray: Option<&Tray<R>>,
    picture: &tauri::image::Image<'_>,
) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL)
        && let Err(err) = window.set_icon(picture.clone().to_owned())
    {
        tracing::warn!(%err, "the window did not take the app icon");
    }
    if let Some(tray) = tray {
        tray.set_app_icon(picture);
    }
}

/// Sets the window title, the tray and the taskbar to show `count` waiting requests, when the badge
/// is on.
fn show_badge<R: Runtime>(
    app: &AppHandle<R>,
    tray: Option<&Tray<R>>,
    count: usize,
    badge_on: bool,
) {
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        // the title is cosmetic; a window that closed meanwhile has nothing to retitle
        let _ = window.set_title(&badge::window_title(count, badge_on));
        #[cfg(windows)]
        {
            let overlay = (badge_on && count > 0).then(|| {
                let number = badge::overlay_number(count);
                tauri::image::Image::new_owned(number.rgba, number.size, number.size)
            });
            // the badge is cosmetic; a window that closed meanwhile has nothing to badge
            let _ = window.set_overlay_icon(overlay);
        }
    }
    if let Some(tray) = tray {
        tray.show(count, badge_on);
    }
}

/// What the event loop keeps between events.
struct Loop {
    out: Outputs,
    store: Arc<Store>,
    settings: Arc<dyn SettingsRepo>,
    open: OpenRequests,
    notifier: Notifier,
    /// What the user calls a workspace, asked before its event is turned into a notice.
    labels: WorkspaceLabel,
    prefs: UiPrefs,
    /// The badge as last shown, so only a change reaches the outputs.
    shown: (usize, bool),
    /// The app icon as last set, so only a change is drawn and set again.
    icon: Wanted,
    /// The start-at-sign-in switch as last applied, so only a change reaches the system's entry;
    /// `None` until the settings have been read once.
    login: Option<bool>,
    /// How often the settings are read while no event arrives.
    poll: Duration,
}

impl Loop {
    fn deliver(&self, notices: Vec<Notice>) {
        for notice in notices {
            (self.out.notify)(&notice, self.prefs.sound());
        }
    }

    fn refresh_badge(&mut self) {
        let now = (self.open.count(), self.prefs.badge());
        if now != self.shown {
            self.shown = now;
            (self.out.badge)(now.0, now.1);
        }
    }

    fn refresh_login(&mut self) {
        let wanted = self.prefs.start_at_login();
        if self.login != Some(wanted) {
            self.login = Some(wanted);
            (self.out.login)(wanted);
        }
    }

    async fn reload_prefs(&mut self) {
        let repo = self.settings.clone();
        match tokio::task::spawn_blocking(move || read_prefs(repo.as_ref())).await {
            Ok(Ok(prefs)) => {
                self.prefs = prefs;
                self.refresh_login();
            }
            Ok(Err(reason)) => {
                tracing::debug!(%reason, "keeping the window preferences as they were");
            }
            Err(err) => tracing::debug!(%err, "the settings read did not finish"),
        }
    }

    /// Sets the app icon again when the choice, or the user's own image, changed.
    async fn reload_icon(&mut self) {
        let (repo, prefs, shown) = (self.settings.clone(), self.prefs.clone(), self.icon.clone());
        let read = tokio::task::spawn_blocking(move || {
            let wanted = Wanted::read(repo.as_ref(), &prefs);
            if wanted == shown {
                return None;
            }
            wanted.draw().map(|drawn| (wanted, drawn))
        })
        .await;
        match read {
            Ok(Some((wanted, drawn))) => {
                (self.out.icon)(&drawn);
                self.icon = wanted;
            }
            Ok(None) => {}
            Err(err) => tracing::debug!(%err, "the app icon read did not finish"),
        }
    }

    async fn on_event(&mut self, event: &Event) {
        self.reload_prefs().await;
        self.reload_icon().await;
        if self.open.apply(event) {
            self.refresh_badge();
        }
        if let Some(workspace) = notify::workspace_of(event)
            && let Some(label) = (self.labels)(workspace.clone()).await
        {
            self.notifier.name(workspace, &label);
        }
        let front = (self.out.in_front)();
        let notices = self
            .notifier
            .handle(Instant::now(), &self.prefs, front, event);
        self.deliver(notices);
    }

    /// The event stream lost events: count again from the store.
    async fn resync(&mut self) {
        let store = self.store.clone();
        if let Ok(open) = tokio::task::spawn_blocking(move || open_requests(&store)).await {
            self.open = open;
            self.refresh_badge();
        }
    }

    /// Follows `stream` until the hub is gone.
    async fn run(mut self, mut stream: Receiver<Event>) {
        // The first tick would be at once; the settings were just read.
        let mut poll = tokio::time::interval_at(tokio::time::Instant::now() + self.poll, self.poll);
        loop {
            let due = self.notifier.deadline().map(tokio::time::Instant::from_std);
            tokio::select! {
                received = stream.recv() => match received {
                    Ok(event) => self.on_event(&event).await,
                    Err(RecvError::Lagged(_)) => self.resync().await,
                    Err(RecvError::Closed) => break,
                },
                () = async {
                    match due {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                } => {
                    let front = (self.out.in_front)();
                    let notices = self.notifier.flush(Instant::now(), &self.prefs, front);
                    self.deliver(notices);
                }
                _ = poll.tick() => {
                    self.reload_prefs().await;
                    self.reload_icon().await;
                    self.refresh_badge();
                }
            }
        }
    }
}

/// Starts the desktop behaviour on a started backend whose main window is open: the tray, the close
/// and minimize buttons, the badge, and the notifications. `marker_folder` is where the once-only
/// tray notice is remembered (`None`: this run only). `login` is the system's start-at-sign-in
/// entry the Settings switch is applied to (`None`: leave it alone). Returns whether the tray
/// icon is there.
pub fn attach<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    backend: &Backend,
    marker_folder: Option<PathBuf>,
    login: Option<Arc<dyn LoginEntry>>,
) -> TrayState {
    let prefs = prefs_or_default(backend.settings().as_ref());
    let open = open_requests(backend.store());
    let first = Wanted::read(backend.settings().as_ref(), &prefs)
        .draw()
        .map(|drawn| icon::image(&drawn))
        .or_else(|| {
            app.default_window_icon()
                .map(|bundled| bundled.clone().to_owned())
        });
    let tray = first
        .and_then(|first| Tray::build(app, &first, open.count(), prefs.badge()))
        .map(Arc::new);
    let tray_state = if tray.is_some() {
        TrayState::Shown
    } else {
        TrayState::Missing
    };
    let out = Outputs::system(app, tray, login);

    let (handle, base) = (app.clone(), backend.url().clone());
    let registered = app
        .notification()
        .on_action(move |performed| on_tap(&handle, &base, performed));
    if let Err(err) = registered {
        tracing::warn!(%err, "a click on a notification will not open the inbox");
    }

    let notify = out.notify.clone();
    attach_with(
        window,
        backend,
        marker_folder,
        tray_state,
        out,
        Arc::new(move |notice| notify(notice, false)),
    );
    tray_state
}

/// [`attach`] with its outputs given: the window's buttons and the event loop.
fn attach_with<R: Runtime>(
    window: &WebviewWindow<R>,
    backend: &Backend,
    marker_folder: Option<PathBuf>,
    tray: TrayState,
    out: Outputs,
    tell: Arc<dyn Fn(&Notice) + Send + Sync>,
) {
    // Subscribe before counting: an event between the two would be missed otherwise.
    let stream = backend.events().subscribe();
    let settings = backend.settings().clone();
    // Settings that cannot be read must not turn the sign-in entry off: it waits for a good read.
    let read = read_prefs(settings.as_ref());
    let login = read.as_ref().ok().map(UiPrefs::start_at_login);
    let prefs = read.unwrap_or_else(|reason| {
        tracing::warn!(%reason, "using the default window preferences");
        UiPrefs::default()
    });
    let open = open_requests(backend.store());
    let buttons = Buttons {
        settings: settings.clone(),
        tray,
        marker: marker_folder,
        told: AtomicBool::new(false),
        tell,
    };
    let button_window = window.clone();
    window.on_window_event(move |event| on_window_event(&button_window, event, &buttons));

    let shown = (open.count(), prefs.badge());
    (out.badge)(shown.0, shown.1);
    let icon = Wanted::read(settings.as_ref(), &prefs);
    if let Some(drawn) = icon.draw() {
        (out.icon)(&drawn);
    }
    // Applied at every start, so a moved or reinstalled program is found again by the entry.
    if let Some(on) = login {
        (out.login)(on);
    }
    let state = Loop {
        out,
        store: backend.store().clone(),
        settings,
        open,
        notifier: Notifier::default(),
        labels: backend.label(),
        prefs,
        shown,
        icon,
        login,
        poll: SETTINGS_POLL,
    };
    tauri::async_runtime::spawn(state.run(stream));
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "fixture"))]
mod behaviour_tests;
