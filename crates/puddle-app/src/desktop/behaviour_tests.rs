// SPDX-License-Identifier: AGPL-3.0-or-later
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;

use puddle_test_kit::eventually_blocking;
use puddle_types::{DiskKind, EventSink, PendingEnd, PendingSummary, WorkspaceId, WorkspaceStatus};
use serde_json::json;
use tauri::test::{MockRuntime, mock_builder};
use tauri::{WebviewUrl, WebviewWindowBuilder};

use super::*;
use crate::autostart::memory::Fake;
use crate::backend::tests::start_fixture;
use crate::shell::{context, with_plugins};

/// What the outputs were asked to show.
#[derive(Default)]
struct Recorded {
    notices: Mutex<Vec<(Notice, bool)>>,
    badges: Mutex<Vec<(usize, bool)>>,
    icons: Mutex<Vec<puddle_appicon::Icon>>,
    logins: Mutex<Vec<bool>>,
    told: AtomicUsize,
    in_front: AtomicBool,
}

impl Recorded {
    fn outputs(self: &Arc<Self>) -> Outputs {
        let (front, notices, badges, icons, logins) = (
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
            self.clone(),
        );
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
            icon: Arc::new(move |icon| icons.icons.lock().unwrap().push(icon.clone())),
            login: Arc::new(move |on| logins.logins.lock().unwrap().push(on)),
        }
    }

    fn notices(&self) -> Vec<(Notice, bool)> {
        self.notices.lock().unwrap().clone()
    }

    fn badges(&self) -> Vec<(usize, bool)> {
        self.badges.lock().unwrap().clone()
    }

    fn icons(&self) -> Vec<puddle_appicon::Icon> {
        self.icons.lock().unwrap().clone()
    }

    fn logins(&self) -> Vec<bool> {
        self.logins.lock().unwrap().clone()
    }
}

struct Rig {
    app: tauri::App<MockRuntime>,
    window: WebviewWindow<MockRuntime>,
    backend: Backend,
    recorded: Arc<Recorded>,
}

fn rig(tray: TrayState) -> Rig {
    let app = with_plugins(mock_builder()).build(context()).unwrap();
    let window = WebviewWindowBuilder::new(&app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
        .build()
        .unwrap();
    let backend = tauri::async_runtime::block_on(start_fixture(0));
    let recorded = Arc::new(Recorded::default());
    attach_with(
        &window,
        &backend,
        None,
        tray,
        recorded.outputs(),
        Arc::new(|_| {}),
    );
    Rig {
        app,
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

fn workspace() -> WorkspaceId {
    WorkspaceId::new("demo").unwrap()
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
    eventually_blocking("the event had no effect", Duration::from_secs(5), || {
        done(&rig.recorded)
    });
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
        &json!({"notifications": false, "notify_oom": false, "notify_disk_full": false, "notify_unexpected_stop": false}),
    );
    settled_after_quiet(&rig, opened(902));
    settled_after_quiet(&rig, Event::oom_kill(workspace(), 3, "node"));
    settled_after_quiet(
        &rig,
        Event::DiskNearlyFull {
            workspace: workspace(),
            disk: DiskKind::Volume,
            percent: 97,
        },
    );
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

#[test]
fn low_free_space_notifies_by_its_own_switch() {
    let low = || Event::DiskSpaceChanged {
        level: puddle_types::DiskSpaceLevel::Low,
        free_mib: 12_288,
    };
    let rig = rig(TrayState::Shown);
    // The request switch is off too: the probe event the helper sends would notify otherwise.
    save(
        &rig,
        &json!({"notify_low_disk_space": false, "notifications": false}),
    );
    settled_after_quiet(&rig, low());
    assert_eq!(rig.recorded.notices(), []);

    // Back to fine, then low again with the switch on: the crossing is announced.
    rig.backend.events().emit(Event::DiskSpaceChanged {
        level: puddle_types::DiskSpaceLevel::Ok,
        free_mib: 40_960,
    });
    save(&rig, &json!({}));
    emit_until(&rig, low(), |r| !r.notices().is_empty());
    let (notice, _) = rig.recorded.notices().remove(0);
    assert_eq!(notice.title, "Disk space is running low");
    assert!(notice.body.starts_with("12 GB are left"), "{}", notice.body);
}

/// An event that must raise nothing: afterwards, a probe event (the badge changes on it) shows the
/// loop got past it.
fn settled_after_quiet(rig: &Rig, event: Event) {
    let before = rig.recorded.badges().len();
    rig.backend.events().emit(event);
    let probe = opened(10_000 + i64::try_from(before).unwrap());
    rig.backend.events().emit(probe);
    eventually_blocking(
        "the loop did not get past the event",
        Duration::from_secs(5),
        || rig.recorded.badges().len() > before,
    );
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
fn minimizing_hides_the_window_only_when_settings_send_it_to_the_tray() {
    let rig = rig(TrayState::Shown);
    let buttons = buttons(&rig, TrayState::Shown);
    assert!(!buttons.on_minimize(&rig.window), "minimize_to_tray is off");
    save(&rig, &json!({"minimize_to_tray": true}));
    assert!(buttons.on_minimize(&rig.window), "minimize_to_tray is on");
}

#[test]
fn without_a_tray_minimizing_never_hides_the_window() {
    let rig = rig(TrayState::Missing);
    let buttons = buttons(&rig, TrayState::Missing);
    save(&rig, &json!({"minimize_to_tray": true}));
    assert!(!buttons.on_minimize(&rig.window));
}

/// A loop over `hub`'s events with the rig's recorder as outputs, started as the app starts it.
fn start_loop(rig: &Rig, hub: &Arc<puddle_api::EventHub>, notifier: Notifier, poll: Duration) {
    start_loop_naming(rig, hub, notifier, poll, rig.backend.label());
}

/// `start_loop` with `labels` as the way to name a workspace.
fn start_loop_naming(
    rig: &Rig,
    hub: &Arc<puddle_api::EventHub>,
    notifier: Notifier,
    poll: Duration,
    labels: WorkspaceLabel,
) {
    let stream = hub.subscribe();
    let settings = rig.backend.settings().clone();
    let prefs = prefs_or_default(settings.as_ref());
    let open = open_requests(rig.backend.store());
    let shown = (open.count(), prefs.badge());
    let icon = Wanted::read(settings.as_ref(), &prefs);
    let login = Some(prefs.start_at_login());
    let state = Loop {
        out: rig.recorded.outputs(),
        store: rig.backend.store().clone(),
        settings,
        open,
        notifier,
        labels,
        prefs,
        shown,
        icon,
        login,
        poll,
    };
    tauri::async_runtime::spawn(state.run(stream));
}

#[test]
fn events_the_stream_dropped_are_made_up_by_counting_the_store_again() {
    let rig = rig(TrayState::Shown);
    let hub = Arc::new(puddle_api::EventHub::new(2));
    let stream = hub.subscribe();
    for _ in 0..10 {
        hub.emit(Event::RulesChanged {});
    }
    // The loop starts believing nothing waits; the store has the fixture's request.
    let state = Loop {
        out: rig.recorded.outputs(),
        store: rig.backend.store().clone(),
        settings: rig.backend.settings().clone(),
        open: OpenRequests::default(),
        notifier: Notifier::default(),
        labels: rig.backend.label(),
        prefs: UiPrefs::default(),
        shown: (0, true),
        icon: Wanted::read(rig.backend.settings().as_ref(), &UiPrefs::default()),
        login: Some(false),
        poll: Duration::from_secs(60),
    };
    tauri::async_runtime::spawn(state.run(stream));
    // The rig's own start-up showed (1, true); the recount shows it again after the loss.
    eventually_blocking("the recount", Duration::from_secs(5), || {
        rig.recorded.badges().len() == 2
    });
    assert_eq!(rig.recorded.badges().last(), Some(&(1, true)));
}

#[test]
fn requests_held_in_a_window_become_one_summary_when_it_ends() {
    let rig = rig(TrayState::Shown);
    let hub = Arc::new(puddle_api::EventHub::default());
    start_loop(
        &rig,
        &hub,
        Notifier::with_window(Duration::from_millis(200)),
        Duration::from_secs(60),
    );
    hub.emit(opened(910));
    hub.emit(opened(911));
    eventually_blocking("the summary", Duration::from_secs(5), || {
        rig.recorded.notices().len() == 2
    });
    let notices = rig.recorded.notices();
    assert_eq!(notices[0].0.request, Some(910));
    assert_eq!(notices[1].0.title, "demo is asking for 1 more");
    assert_eq!(notices[1].0.request, Some(911));
}

#[test]
fn a_notice_names_the_workspace_by_the_label_the_backend_finds() {
    let rig = rig(TrayState::Shown);
    let hub = Arc::new(puddle_api::EventHub::default());
    start_loop_naming(
        &rig,
        &hub,
        Notifier::default(),
        Duration::from_secs(60),
        Arc::new(|_| Box::pin(async { Some("Demo project".to_owned()) })),
    );
    hub.emit(opened(920));
    eventually_blocking("the notice", Duration::from_secs(5), || {
        !rig.recorded.notices().is_empty()
    });
    let title = rig.recorded.notices()[0].0.title.clone();
    assert!(title.contains("Demo project"), "{title}");
}

#[test]
fn a_switch_changed_in_settings_reaches_the_badge_without_any_event() {
    let rig = rig(TrayState::Shown);
    let hub = Arc::new(puddle_api::EventHub::default());
    start_loop(&rig, &hub, Notifier::default(), Duration::from_millis(50));
    save(&rig, &json!({"badge": false}));
    eventually_blocking("the badge going off", Duration::from_secs(5), || {
        rig.recorded.badges().last() == Some(&(1, false))
    });
}

#[test]
fn the_window_takes_the_app_icon_and_a_missing_window_or_tray_is_no_error() {
    let rig = rig(TrayState::Missing);
    let picture = icon::image(&drawn(puddle_settings::AppIconChoice::DuckPond));
    show_icon(rig.app.handle(), None, &picture);
    let empty = with_plugins(mock_builder()).build(context()).unwrap();
    show_icon(empty.handle(), None, &picture);
}

fn drawn(choice: puddle_settings::AppIconChoice) -> puddle_appicon::Icon {
    puddle_appicon::built_in(choice, puddle_appicon::SIZE).unwrap()
}

#[test]
fn the_app_icon_is_set_at_start_and_follows_the_choice_in_settings_without_any_event() {
    use puddle_settings::AppIconChoice::{Duck, DuckNight, DuckSun};
    let rig = rig(TrayState::Shown);
    assert_eq!(rig.recorded.icons(), [drawn(Duck)], "the duck at start");

    let hub = Arc::new(puddle_api::EventHub::default());
    start_loop(&rig, &hub, Notifier::default(), Duration::from_millis(50));
    save(&rig, &json!({"app_icon": "duck_night"}));
    eventually_blocking("the night duck", Duration::from_secs(5), || {
        rig.recorded.icons().last() == Some(&drawn(DuckNight))
    });
    // An event reads the settings too, so the next choice shows on it, not only on the poll.
    save(&rig, &json!({"app_icon": "duck_sun"}));
    eventually_blocking("the sun duck", Duration::from_secs(5), || {
        rig.recorded.icons().last() == Some(&drawn(DuckSun))
    });
}

#[test]
fn your_own_image_is_set_when_chosen_and_again_when_replaced_and_a_vanished_one_is_the_duck() {
    use puddle_settings::AppIconChoice::Duck;
    let rig = rig(TrayState::Shown);
    let hub = Arc::new(puddle_api::EventHub::default());
    start_loop(&rig, &hub, Notifier::default(), Duration::from_millis(50));
    let colour = |rgba: [u8; 4]| puddle_appicon::Icon {
        size: puddle_appicon::SIZE,
        rgba: rgba.repeat((puddle_appicon::SIZE * puddle_appicon::SIZE) as usize),
    };
    let (red, blue) = (colour([200, 0, 0, 255]), colour([0, 0, 200, 255]));

    let settings = rig.backend.settings();
    settings.save_app_icon(&red.to_png().unwrap()).unwrap();
    save(&rig, &json!({"app_icon": "custom"}));
    eventually_blocking("the red image", Duration::from_secs(5), || {
        rig.recorded.icons().last() == Some(&red)
    });

    settings.save_app_icon(&blue.to_png().unwrap()).unwrap();
    eventually_blocking("the blue image", Duration::from_secs(5), || {
        rig.recorded.icons().last() == Some(&blue)
    });

    // The file went away behind our back: the window keeps an icon.
    settings.delete_app_icon().unwrap();
    eventually_blocking("the duck again", Duration::from_secs(5), || {
        rig.recorded.icons().last() == Some(&drawn(Duck))
    });
}

#[test]
fn settings_that_cannot_be_read_leave_the_window_preferences_as_they_were() {
    let rig = rig(TrayState::Shown);
    save(&rig, &json!({"sound": true}));
    let hub = Arc::new(puddle_api::EventHub::default());
    start_loop(&rig, &hub, Notifier::default(), Duration::from_secs(60));
    hub.emit(opened(920));
    eventually_blocking("the first notice", Duration::from_secs(5), || {
        rig.recorded.notices().len() == 1
    });
    assert!(rig.recorded.notices()[0].1, "sound is on");
    // Now the document is damaged: the next event is still handled, with the last good ones.
    save(&rig, &json!({"close_behaviour": "explode"}));
    hub.emit(Event::oom_kill(workspace(), 3, "node"));
    eventually_blocking("the second notice", Duration::from_secs(5), || {
        rig.recorded.notices().len() == 2
    });
    assert!(rig.recorded.notices()[1].1, "the sound switch stayed on");
}

#[test]
fn the_system_outputs_ask_the_window_and_show_the_badge_on_its_title() {
    let rig = rig(TrayState::Shown);
    let out = Outputs::system(rig.app.handle(), None, None);
    // The test window is visible and not minimized, and nobody has focused it.
    assert!(!(out.in_front)());
    (out.badge)(2, true);
    (out.badge)(0, false);
    (out.badge)(12, true);
}

#[test]
fn the_system_outputs_make_the_entry_match_the_switch_for_this_program() {
    let rig = rig(TrayState::Shown);
    let entry = Arc::new(Fake::default());
    let out = Outputs::system(rig.app.handle(), None, Some(entry.clone()));
    let exe = std::env::current_exe().unwrap();
    (out.login)(true);
    assert_eq!(entry.now(), Some(autostart::command_for(&exe)));
    (out.login)(false);
    assert_eq!(entry.now(), None);
}

#[test]
fn an_entry_that_cannot_be_changed_is_logged_and_does_not_stop_anything() {
    let entry = autostart::Unsupported("test");
    set_login(&entry, true, Ok(PathBuf::from("/x/puddle-app")));
    let fake = Fake::default();
    set_login(&fake, true, Err(std::io::Error::other("no such program")));
    assert_eq!(fake.changes(), 0, "without its own path nothing is written");
}

#[test]
fn the_start_at_sign_in_switch_is_applied_at_start_and_again_only_when_it_changes() {
    let rig = rig(TrayState::Shown);
    // Off by default: the entry is brought to "off" once at start.
    assert_eq!(rig.recorded.logins(), [false]);
    // A loop of its own with its own recorder, so the rig's loop adds nothing to what is counted.
    let own = Arc::new(Recorded::default());
    let hub = Arc::new(puddle_api::EventHub::default());
    let settings = rig.backend.settings().clone();
    let prefs = prefs_or_default(settings.as_ref());
    let state = Loop {
        out: own.outputs(),
        store: rig.backend.store().clone(),
        open: open_requests(rig.backend.store()),
        notifier: Notifier::default(),
        labels: rig.backend.label(),
        shown: (1, prefs.badge()),
        icon: Wanted::read(settings.as_ref(), &prefs),
        login: Some(prefs.start_at_login()),
        settings,
        prefs,
        poll: Duration::from_millis(50),
    };
    tauri::async_runtime::spawn(state.run(hub.subscribe()));
    save(&rig, &json!({"start_at_login": true}));
    eventually_blocking(
        "the switch reaching the entry",
        Duration::from_secs(5),
        || own.logins() == [true],
    );
    // Events and polls that read the unchanged switch apply nothing more.
    hub.emit(opened(930));
    hub.emit(opened(931));
    eventually_blocking("the loop past the events", Duration::from_secs(5), || {
        own.badges().len() == 2
    });
    assert_eq!(own.logins(), [true]);
    save(&rig, &json!({"start_at_login": false}));
    eventually_blocking("the switch going off", Duration::from_secs(5), || {
        own.logins() == [true, false]
    });
}

#[test]
fn settings_that_cannot_be_read_at_the_start_leave_the_entry_until_a_good_read() {
    let app = with_plugins(mock_builder()).build(context()).unwrap();
    let window = WebviewWindowBuilder::new(&app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
        .build()
        .unwrap();
    let backend = tauri::async_runtime::block_on(start_fixture(0));
    backend
        .settings()
        .save_global(json!({"ui": {"close_behaviour": "explode"}}))
        .unwrap();
    let recorded = Arc::new(Recorded::default());
    attach_with(
        &window,
        &backend,
        None,
        TrayState::Shown,
        recorded.outputs(),
        Arc::new(|_| {}),
    );
    assert_eq!(recorded.logins(), Vec::<bool>::new(), "nothing was applied");
    backend
        .settings()
        .save_global(json!({"schema_version": 2, "ui": {"start_at_login": true}}))
        .unwrap();
    eventually_blocking(
        "the first good read applying the switch",
        Duration::from_secs(10),
        || recorded.logins() == [true],
    );
}

#[test]
fn a_switch_that_was_on_before_the_start_is_applied_at_the_start() {
    let app = with_plugins(mock_builder()).build(context()).unwrap();
    let window = WebviewWindowBuilder::new(&app, MAIN_LABEL, WebviewUrl::App("index.html".into()))
        .build()
        .unwrap();
    let backend = tauri::async_runtime::block_on(start_fixture(0));
    backend
        .settings()
        .save_global(json!({"schema_version": 2, "ui": {"start_at_login": true}}))
        .unwrap();
    let recorded = Arc::new(Recorded::default());
    attach_with(
        &window,
        &backend,
        None,
        TrayState::Shown,
        recorded.outputs(),
        Arc::new(|_| {}),
    );
    assert_eq!(recorded.logins(), [true]);
}

#[test]
fn a_start_at_sign_in_hides_the_window_to_the_tray_or_minimizes_it_without_one() {
    assert_eq!(login_start(TrayState::Shown), LoginStart::HideToTray);
    assert_eq!(login_start(TrayState::Missing), LoginStart::Minimize);
    // The mock window cannot report being hidden or minimized, so this only runs both calls.
    for tray in [TrayState::Shown, TrayState::Missing] {
        start_out_of_the_way(&rig(tray).window, tray);
    }
}

#[test]
fn a_click_opens_the_inbox_in_the_main_window() {
    let rig = rig(TrayState::Shown);
    let base = Url::parse("http://127.0.0.1:4000/").unwrap();
    open_inbox(rig.app.handle(), &base, Opens::Request(3));
    open_inbox(rig.app.handle(), &base, Opens::Inbox);
}

fn performed(action: &str, request: Option<i64>) -> ActionPerformed {
    let extra = request.map_or_else(|| json!({}), |id| json!({ REQUEST_EXTRA: id }));
    serde_json::from_value(json!({
        "actionId": action,
        "notification": { "id": 1, "extra": extra },
    }))
    .unwrap()
}

#[test]
fn only_a_click_on_the_notification_itself_opens_anything() {
    let rig = rig(TrayState::Shown);
    assert_eq!(tapped(&performed("tap", Some(7))), Some(Opens::Request(7)));
    assert_eq!(tapped(&performed("tap", None)), Some(Opens::Inbox));
    assert_eq!(tapped(&performed("dismiss", Some(7))), None);
    let unreadable: ActionPerformed =
        serde_json::from_value(json!({"actionId": "tap", "notification": "garbled"})).unwrap();
    assert_eq!(tapped(&unreadable), Some(Opens::Inbox));
    // Both ends run on the test runtime: a click navigates the window, any other action does not.
    let app = rig.app.handle();
    let base = Url::parse("http://127.0.0.1:4000/").unwrap();
    on_tap(app, &base, &performed("tap", Some(7)));
    on_tap(app, &base, &performed("dismiss", None));
}
