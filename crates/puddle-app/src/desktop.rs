// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_store::Store;
use puddle_types::Event;
use serde_json::Value;
use tauri::menu::{Menu, MenuItem, Submenu};
use tauri::{AppHandle, Manager, Runtime, WebviewWindow, WindowEvent};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::broadcast::Receiver;
use tokio::sync::broadcast::error::RecvError;
use url::Url;

use crate::backend::Backend;
use crate::badge::{self, OpenRequests};
use crate::notify::{self, Notice, Notifier, REQUEST_EXTRA};
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

/// The address a notification opens: the inbox, at `request` when the notification was about one.
#[must_use]
pub fn inbox_url(base: &Url, request: Option<i64>) -> Url {
    let mut url = base.clone();
    url.set_path("/inbox");
    url.set_query(request.map(|id| format!("request={id}")).as_deref());
    url
}

/// Brings the main window up on the inbox, at `request` if there is one.
fn open_inbox<R: Runtime>(app: &AppHandle<R>, base: &Url, request: Option<i64>) {
    crate::shell::focus_main(app);
    if let Some(window) = app.get_webview_window(MAIN_LABEL)
        && let Err(err) = window.navigate(inbox_url(base, request))
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
    match store.open_pending(None) {
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
                let _ = window.minimize();
                tracing::info!("there is no tray icon, so the close button minimized the window");
                true
            }
        }
    }

    /// The window was minimized: hide it too when Settings say minimizing goes to the tray.
    fn on_minimize<R: Runtime>(&self, window: &WebviewWindow<R>) {
        let prefs = prefs_or_default(self.settings.as_ref());
        if tray::minimize_action(&prefs, self.tray) == MinimizeAction::HideToTray {
            let _ = window.hide();
        }
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
            app.exit(0);
        }
    });
    if let Some(window) = app.get_webview_window(MAIN_LABEL) {
        window.hide_menu()?;
    }
    Ok(())
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
}

impl Outputs {
    /// The real outputs: the main window, the notification plugin and `tray`.
    fn system<R: Runtime>(app: &AppHandle<R>, tray: Option<Arc<Tray<R>>>) -> Self {
        let front = app.clone();
        let notifying = app.clone();
        let showing = app.clone();
        Self {
            in_front: Arc::new(move || in_front(&front)),
            notify: Arc::new(move |notice, sound| show_notice(&notifying, notice, sound)),
            badge: Arc::new(move |count, on| show_badge(&showing, tray.as_deref(), count, on)),
        }
    }
}

fn show_notice<R: Runtime>(app: &AppHandle<R>, notice: &Notice, sound: bool) {
    if let Err(err) = notify::show(app, notice, sound) {
        tracing::warn!(%err, "could not show a notification");
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
        let _ = window.set_title(&badge::window_title(count, badge_on));
        #[cfg(windows)]
        {
            let overlay = (badge_on && count > 0).then(|| {
                let dot = badge::overlay_dot();
                tauri::image::Image::new_owned(dot.rgba, dot.size, dot.size)
            });
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
    prefs: UiPrefs,
    /// The badge as last shown, so only a change reaches the outputs.
    shown: (usize, bool),
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

    async fn reload_prefs(&mut self) {
        let repo = self.settings.clone();
        match tokio::task::spawn_blocking(move || read_prefs(repo.as_ref())).await {
            Ok(Ok(prefs)) => self.prefs = prefs,
            Ok(Err(reason)) => {
                tracing::debug!(%reason, "keeping the window preferences as they were");
            }
            Err(err) => tracing::debug!(%err, "the settings read did not finish"),
        }
    }

    async fn on_event(&mut self, event: &Event) {
        self.reload_prefs().await;
        if self.open.apply(event) {
            self.refresh_badge();
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
                    self.refresh_badge();
                }
            }
        }
    }
}

/// Starts the desktop behaviour on a started backend whose main window is open: the tray, the close
/// and minimize buttons, the badge, and the notifications. `marker_folder` is where the once-only
/// tray notice is remembered (`None`: this run only).
pub fn attach<R: Runtime>(
    app: &AppHandle<R>,
    window: &WebviewWindow<R>,
    backend: &Backend,
    marker_folder: Option<PathBuf>,
) {
    let prefs = prefs_or_default(backend.settings().as_ref());
    let open = open_requests(backend.store());
    let tray = Tray::build(app, open.count(), prefs.badge()).map(Arc::new);
    let tray_state = if tray.is_some() {
        TrayState::Shown
    } else {
        TrayState::Missing
    };
    let out = Outputs::system(app, tray);

    let (handle, base) = (app.clone(), backend.url().clone());
    let registered = app.notification().on_action(move |performed| {
        if performed.action_id() == "tap" {
            let request = performed
                .notification()
                .and_then(|n| n.extra().get(REQUEST_EXTRA))
                .and_then(Value::as_i64);
            open_inbox(&handle, &base, request);
        }
    });
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
    let prefs = prefs_or_default(settings.as_ref());
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
    let state = Loop {
        out,
        store: backend.store().clone(),
        settings,
        open,
        notifier: Notifier::default(),
        prefs,
        shown,
        poll: SETTINGS_POLL,
    };
    tauri::async_runtime::spawn(state.run(stream));
}

#[cfg(test)]
mod tests {
    use puddle_api::MemorySettings;
    use serde_json::json;

    use super::*;

    #[test]
    fn a_notification_opens_the_inbox_at_its_request_or_the_inbox_as_it_is() {
        let base = Url::parse("http://127.0.0.1:4000/settings?x=1#top").unwrap();
        assert_eq!(
            inbox_url(&base, Some(42)).as_str(),
            "http://127.0.0.1:4000/inbox?request=42#top"
        );
        assert_eq!(
            inbox_url(&base, None).as_str(),
            "http://127.0.0.1:4000/inbox#top"
        );
    }

    #[test]
    fn the_preferences_come_from_the_global_settings_and_default_without_any() {
        let repo = MemorySettings::default();
        assert_eq!(read_prefs(&repo).unwrap(), UiPrefs::default());
        repo.save_global(json!({"schema_version": 2, "ui": {"close_behaviour": "quit", "minimize_to_tray": true}}))
            .unwrap();
        let prefs = read_prefs(&repo).unwrap();
        assert_eq!(
            prefs.close_behaviour(),
            puddle_settings::CloseBehaviour::Quit
        );
        assert!(prefs.minimize_to_tray());
    }

    #[test]
    fn settings_that_are_not_valid_say_so_and_the_window_still_gets_defaults() {
        let repo = MemorySettings::default();
        repo.save_global(json!({"ui": {"close_behaviour": "explode"}}))
            .unwrap();
        let reason = read_prefs(&repo).unwrap_err();
        assert!(reason.starts_with("the settings are not valid"), "{reason}");
        assert_eq!(prefs_or_default(&repo), UiPrefs::default());
    }

    #[test]
    fn the_tray_notice_is_once_per_data_folder_and_once_per_run_without_one() {
        let folder = tempfile::tempdir().unwrap();
        let run = AtomicBool::new(false);
        assert!(first_time(Some(folder.path()), &run));
        assert!(!first_time(Some(folder.path()), &run));
        // A new run, same folder: still told.
        assert!(!first_time(Some(folder.path()), &AtomicBool::new(false)));

        let without = AtomicBool::new(false);
        assert!(first_time(None, &without));
        assert!(!first_time(None, &without));
        // A folder that cannot be written falls back to this run.
        let missing = folder.path().join("gone");
        let fallback = AtomicBool::new(false);
        assert!(first_time(Some(&missing), &fallback));
        assert!(!first_time(Some(&missing), &fallback));
    }
}

#[cfg(all(test, feature = "fixture"))]
mod behaviour_tests {
    #![expect(
        clippy::unwrap_used,
        clippy::panic,
        reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
    )]

    use std::sync::Mutex;
    use std::sync::atomic::AtomicUsize;

    use puddle_types::{EventSink, PendingEnd, PendingSummary, WorkspaceName, WorkspaceStatus};
    use serde_json::json;
    use tauri::test::{MockRuntime, mock_builder};
    use tauri::{WebviewUrl, WebviewWindowBuilder};

    use super::*;
    use crate::backend::tests::start_fixture;
    use crate::shell::{context, with_plugins};

    /// What the outputs were asked to show.
    #[derive(Default)]
    struct Recorded {
        notices: Mutex<Vec<(Notice, bool)>>,
        badges: Mutex<Vec<(usize, bool)>>,
        told: AtomicUsize,
        in_front: AtomicBool,
    }

    impl Recorded {
        fn outputs(self: &Arc<Self>) -> Outputs {
            let (front, notices, badges) = (self.clone(), self.clone(), self.clone());
            Outputs {
                in_front: Arc::new(move || front.in_front.load(Ordering::SeqCst)),
                notify: Arc::new(move |notice, sound| {
                    notices
                        .notices
                        .lock()
                        .unwrap()
                        .push((notice.clone(), sound));
                }),
                badge: Arc::new(move |count, on| badges.badges.lock().unwrap().push((count, on))),
            }
        }

        fn notices(&self) -> Vec<(Notice, bool)> {
            self.notices.lock().unwrap().clone()
        }

        fn badges(&self) -> Vec<(usize, bool)> {
            self.badges.lock().unwrap().clone()
        }
    }

    struct Rig {
        /// Kept so the window stays valid.
        _app: tauri::App<MockRuntime>,
        window: WebviewWindow<MockRuntime>,
        backend: Backend,
        recorded: Arc<Recorded>,
    }

    fn rig(tray: TrayState) -> Rig {
        let app = with_plugins(mock_builder()).build(context()).unwrap();
        let window =
            WebviewWindowBuilder::new(&app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
                .build()
                .unwrap();
        let backend = tauri::async_runtime::block_on(start_fixture(0));
        let recorded = Arc::new(Recorded::default());
        let told = recorded.clone();
        attach_with(
            &window,
            &backend,
            None,
            tray,
            recorded.outputs(),
            Arc::new(move |_| {
                told.told.fetch_add(1, Ordering::SeqCst);
            }),
        );
        Rig {
            _app: app,
            window,
            backend,
            recorded,
        }
    }

    fn save(rig: &Rig, ui: &Value) {
        rig.backend
            .settings()
            .save_global(json!({"schema_version": 2, "ui": ui}))
            .unwrap();
    }

    fn workspace() -> WorkspaceName {
        WorkspaceName::new("demo").unwrap()
    }

    fn opened(id: i64) -> Event {
        Event::PendingOpened {
            request: PendingSummary {
                id,
                workspace: workspace(),
                host: "api.example.org".to_owned(),
                registrable_domain: "example.org".to_owned(),
                port: 443,
                first_seen: 1,
                last_seen: 1,
                attempts: 1,
            },
        }
    }

    /// Emits `event` and waits until `done` holds (the loop runs on its own task).
    fn emit_until(rig: &Rig, event: Event, done: impl Fn(&Recorded) -> bool) {
        rig.backend.events().emit(event);
        for _ in 0..200 {
            if done(&rig.recorded) {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the event had no effect in 5 s");
    }

    #[test]
    fn the_badge_starts_at_the_requests_already_waiting_and_follows_the_events() {
        let rig = rig(TrayState::Shown);
        // The fixture seeds one open request.
        assert_eq!(rig.recorded.badges(), [(1, true)]);
        emit_until(&rig, opened(900), |r| r.badges().len() == 2);
        assert_eq!(rig.recorded.badges(), [(1, true), (2, true)]);
        emit_until(
            &rig,
            Event::PendingClosed {
                workspace: workspace(),
                id: 900,
                state: PendingEnd::Allowed,
                rule_id: Some(1),
            },
            |r| r.badges().len() == 3,
        );
        assert_eq!(rig.recorded.badges().last(), Some(&(1, true)));
    }

    #[test]
    fn a_new_request_notifies_and_the_sound_switch_decides_the_sound() {
        let rig = rig(TrayState::Shown);
        emit_until(&rig, opened(901), |r| !r.notices().is_empty());
        let (notice, sound) = rig.recorded.notices().remove(0);
        assert_eq!(notice.request, Some(901));
        assert!(notice.title.contains("demo"), "{}", notice.title);
        assert!(!sound, "the sound is off by default");

        save(&rig, &json!({"sound": true}));
        let other = Event::oom_kill(workspace(), 3, "node");
        emit_until(&rig, other, |r| r.notices().len() == 2);
        assert!(rig.recorded.notices()[1].1, "the sound switch is on");
    }

    #[test]
    fn each_switch_in_settings_silences_its_own_notices_and_a_front_window_silences_all() {
        let rig = rig(TrayState::Shown);
        save(
            &rig,
            &json!({"notifications": false, "notify_oom": false, "notify_unexpected_stop": false}),
        );
        settled_after_quiet(&rig, opened(902));
        settled_after_quiet(&rig, Event::oom_kill(workspace(), 3, "node"));
        settled_after_quiet(
            &rig,
            Event::StatusChanged {
                workspace: workspace(),
                status: WorkspaceStatus::Crashed,
            },
        );
        assert_eq!(rig.recorded.notices(), []);

        // Back on, but the window is in front: nothing shows; with the choice off, it does.
        save(&rig, &json!({}));
        rig.recorded.in_front.store(true, Ordering::SeqCst);
        settled_after_quiet(&rig, opened(903));
        assert_eq!(rig.recorded.notices(), []);
        save(&rig, &json!({"notify_only_unfocused": false}));
        emit_until(&rig, opened(904), |r| !r.notices().is_empty());
    }

    /// An event that must raise nothing: afterwards, a probe event (the badge changes on it) shows the
    /// loop got past it.
    fn settled_after_quiet(rig: &Rig, event: Event) {
        let before = rig.recorded.badges().len();
        rig.backend.events().emit(event);
        let probe = opened(10_000 + i64::try_from(before).unwrap());
        rig.backend.events().emit(probe);
        for _ in 0..200 {
            if rig.recorded.badges().len() > before {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the loop did not get past the event");
    }

    #[test]
    fn the_badge_switch_turns_the_badge_off_everywhere() {
        let rig = rig(TrayState::Shown);
        save(&rig, &json!({"badge": false}));
        emit_until(&rig, opened(905), |r| r.badges().len() == 2);
        assert_eq!(rig.recorded.badges().last(), Some(&(2, false)));
    }

    fn buttons(rig: &Rig, tray: TrayState) -> Buttons {
        let told = rig.recorded.clone();
        Buttons {
            settings: rig.backend.settings().clone(),
            tray,
            marker: None,
            told: AtomicBool::new(false),
            tell: Arc::new(move |_| {
                told.told.fetch_add(1, Ordering::SeqCst);
            }),
        }
    }

    #[test]
    fn the_close_button_hides_to_the_tray_and_says_so_once() {
        let rig = rig(TrayState::Shown);
        let buttons = buttons(&rig, TrayState::Shown);
        assert!(buttons.on_close(&rig.window), "the window stays");
        assert!(buttons.on_close(&rig.window));
        assert_eq!(rig.recorded.told.load(Ordering::SeqCst), 1, "told once");
    }

    #[test]
    fn the_close_button_quits_when_settings_say_so() {
        let rig = rig(TrayState::Shown);
        save(&rig, &json!({"close_behaviour": "quit"}));
        let buttons = buttons(&rig, TrayState::Shown);
        assert!(!buttons.on_close(&rig.window), "the window closes");
        assert_eq!(rig.recorded.told.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn without_a_tray_the_close_button_keeps_the_window_and_tells_nobody() {
        let rig = rig(TrayState::Missing);
        let buttons = buttons(&rig, TrayState::Missing);
        assert!(buttons.on_close(&rig.window));
        assert_eq!(rig.recorded.told.load(Ordering::SeqCst), 0);
        save(&rig, &json!({"close_behaviour": "quit"}));
        assert!(!buttons.on_close(&rig.window), "quit still quits");
    }

    #[test]
    fn minimizing_only_reads_the_settings_to_hide_in_the_tray() {
        let rig = rig(TrayState::Shown);
        let buttons = buttons(&rig, TrayState::Shown);
        buttons.on_minimize(&rig.window);
        save(&rig, &json!({"minimize_to_tray": true}));
        buttons.on_minimize(&rig.window);
        // The test window cannot tell whether it is hidden; the matrix itself is tested in `tray`,
        // and the real window by the Windows smoke test.
    }
}
