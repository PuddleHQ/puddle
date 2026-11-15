// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

fn prefs(close: CloseBehaviour, minimize: bool) -> UiPrefs {
    let mut prefs = UiPrefs::default();
    prefs.close_behaviour = Some(close);
    prefs.minimize_to_tray = Some(minimize);
    prefs
}

#[test]
fn the_close_button_follows_the_setting_and_falls_back_to_minimize_without_a_tray() {
    use CloseAction::{HideToTray, Minimize, Quit};
    use CloseBehaviour::{Quit as ChooseQuit, Tray as ChooseTray};
    use TrayState::{Missing, Shown};
    for (setting, tray, expected) in [
        (ChooseTray, Shown, HideToTray),
        (ChooseTray, Missing, Minimize),
        (ChooseQuit, Shown, Quit),
        (ChooseQuit, Missing, Quit),
    ] {
        assert_eq!(
            close_action(&prefs(setting, false), tray),
            expected,
            "{setting:?} with the tray {tray:?}"
        );
    }
}

#[test]
fn closing_goes_to_the_tray_by_default_and_minimizing_to_the_taskbar() {
    let defaults = UiPrefs::default();
    assert_eq!(
        close_action(&defaults, TrayState::Shown),
        CloseAction::HideToTray
    );
    assert_eq!(
        minimize_action(&defaults, TrayState::Shown),
        MinimizeAction::ToTaskbar
    );
}

#[test]
fn the_four_settings_combinations_and_the_missing_tray_make_the_whole_matrix() {
    use CloseBehaviour::{Quit, Tray as ToTray};
    for (close, minimize, tray, expected_close, expected_minimize) in [
        (
            ToTray,
            false,
            TrayState::Shown,
            CloseAction::HideToTray,
            MinimizeAction::ToTaskbar,
        ),
        (
            ToTray,
            true,
            TrayState::Shown,
            CloseAction::HideToTray,
            MinimizeAction::HideToTray,
        ),
        (
            Quit,
            false,
            TrayState::Shown,
            CloseAction::Quit,
            MinimizeAction::ToTaskbar,
        ),
        (
            Quit,
            true,
            TrayState::Shown,
            CloseAction::Quit,
            MinimizeAction::HideToTray,
        ),
        (
            ToTray,
            true,
            TrayState::Missing,
            CloseAction::Minimize,
            MinimizeAction::ToTaskbar,
        ),
        (
            Quit,
            true,
            TrayState::Missing,
            CloseAction::Quit,
            MinimizeAction::ToTaskbar,
        ),
    ] {
        let p = prefs(close, minimize);
        assert_eq!(
            close_action(&p, tray),
            expected_close,
            "{close:?} {minimize} {tray:?}"
        );
        assert_eq!(
            minimize_action(&p, tray),
            expected_minimize,
            "{close:?} {minimize} {tray:?}"
        );
    }
}

#[test]
fn the_tray_icon_with_a_dot_is_the_app_icon_with_the_badge_drawn_on_it() {
    let app_icon = Image::new_owned(vec![0x20; 32 * 32 * 4], 32, 32);
    let (plain, marked) = icons(&app_icon);
    assert_eq!(plain.rgba(), app_icon.rgba());
    let marked = marked.unwrap();
    assert_eq!((marked.width(), marked.height()), (32, 32));
    assert_ne!(marked.rgba(), plain.rgba());
    // An icon that is not square gets no dot.
    let wide = Image::new_owned(vec![0; 8 * 4 * 4], 8, 4);
    assert!(icons(&wide).1.is_none());
}

/// The tray needs GTK, which needs a display. This test starts a virtual one (`Xvfb`, from the
/// `xvfb` package) and runs itself again on it, so the icon, its menu and its badge are built
/// for real; that needs the tray library too (`libayatana-appindicator3-1`). The inner run is
/// told apart by an environment variable.
#[cfg(target_os = "linux")]
#[test]
fn the_icon_is_made_with_a_menu_and_shows_the_waiting_count() {
    const INNER: &str = "PUDDLE_TRAY_TEST_ON_DISPLAY";
    if std::env::var_os(INNER).is_some() {
        on_display();
        // skip-ok: the inner run of this same test, on the virtual display; it ran the checks above
        return;
    }
    // `-displayfd 1` makes Xvfb pick a free display and print its number once it is ready.
    let mut xvfb = std::process::Command::new("Xvfb")
            .args([
                "-displayfd",
                "1",
                "-screen",
                "0",
                "640x480x24",
                "-nolisten",
                "tcp",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("this test needs Xvfb (apt package xvfb) and libayatana-appindicator3.so.1 (apt package libayatana-appindicator3-1)");
    let mut ready = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(xvfb.stdout.take().unwrap()),
        &mut ready,
    )
    .unwrap();
    let display = format!(":{}", ready.trim());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tray::tests::the_icon_is_made_with_a_menu_and_shows_the_waiting_count",
            "--nocapture",
        ])
        .env(INNER, "1")
        .env("DISPLAY", &display)
        .env_remove("DBUS_SESSION_BUS_ADDRESS")
        .status()
        .unwrap();
    xvfb.kill().unwrap();
    xvfb.wait().unwrap();
    assert!(
        status.success(),
        "the run on the display failed; if the log says libayatana-appindicator3.so.1 cannot be opened, \
             install apt package libayatana-appindicator3-1"
    );
}

/// What the test above runs on the virtual display.
#[cfg(target_os = "linux")]
fn on_display() {
    use tauri::test::mock_builder;

    gtk::init().unwrap();
    let app = crate::shell::with_plugins(mock_builder())
        .build(crate::shell::context())
        .unwrap();
    let icon = Image::new_owned(vec![0x40; 32 * 32 * 4], 32, 32);
    let tray = Tray::make(app.handle(), &icon, 0, true).unwrap();
    assert!(
        tray.look().marked.is_some(),
        "the app icon is square, so it gets a dot"
    );
    assert_eq!(tray.waiting.text().unwrap(), "Nothing is waiting");
    tray.show(3, true);
    assert_eq!(tray.waiting.text().unwrap(), "3 requests are waiting");
    tray.show(3, false);
    tray.show(0, true);
    assert_eq!(tray.waiting.text().unwrap(), "Nothing is waiting");
    // Another app icon keeps the count: with requests waiting it still carries the dot.
    tray.show(2, true);
    let other = Image::new_owned(vec![0xc0; 16 * 16 * 4], 16, 16);
    tray.set_app_icon(&other);
    assert_eq!(tray.waiting.text().unwrap(), "2 requests are waiting");
    assert_eq!(tray.look().plain.rgba(), other.rgba());
    assert_ne!(tray.look().image().rgba(), other.rgba(), "dotted");
    // No session bus here, so there is no tray host: the shell goes without a tray.
    assert!(Tray::build(app.handle(), &icon, 1, true).is_none());
}

#[test]
fn the_look_shows_the_dot_only_while_requests_wait_and_the_badge_is_on() {
    let plain = Image::new_owned(vec![1; 4 * 4 * 4], 4, 4);
    let marked = Image::new_owned(vec![2; 4 * 4 * 4], 4, 4);
    let mut look = Look {
        plain: plain.clone(),
        marked: Some(marked.clone()),
        waiting: 0,
        badge_on: true,
    };
    assert_eq!(look.image().rgba(), plain.rgba(), "nothing waits");
    look.waiting = 2;
    assert_eq!(look.image().rgba(), marked.rgba());
    look.badge_on = false;
    assert_eq!(look.image().rgba(), plain.rgba(), "the badge is off");
    look.badge_on = true;
    look.marked = None;
    assert_eq!(look.image().rgba(), plain.rgba(), "no dot can be drawn");
}

#[test]
fn a_panic_while_making_the_icon_is_an_answer_not_a_crash() {
    assert_eq!(guarded(|| 7), Some(7));
    let none: Option<u8> = guarded(|| panic!("no library"));
    assert_eq!(none, None);
}

// Raced before the swaps shared one lock (red 19 of 20 runs on 2 CPUs): the icon's restore
// overwrote a crash hook installed at the same moment. The rounds chain a hook that only sets
// a flag, as `crash::install` does through `panic_hook::chain`: the real one writes a crash
// file with a backtrace and an fsync per panic, and under a busy disk 300 of those took over
// 120 s. A silent base hook keeps the closing panic off stderr. One last round uses
// the real `crash::install`.
#[test]
fn a_hook_installed_while_the_icon_is_made_survives_it() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let silent = || std::panic::set_hook(Box::new(|_| {}));
    silent();
    for round in 0..300 {
        let ran = Arc::new(AtomicBool::new(false));
        let seen = Arc::clone(&ran);
        let barrier = std::sync::Barrier::new(2);
        let barrier = &barrier;
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                assert_eq!(guarded(|| 1), Some(1));
            });
            scope.spawn(move || {
                barrier.wait();
                puddle_types::panic_hook::chain(move |_| seen.store(true, Ordering::SeqCst));
            });
        });
        let _ = std::thread::spawn(|| panic!("after both")).join();
        assert!(
            ran.load(Ordering::SeqCst),
            "round {round}: the chained hook was lost"
        );
        silent();
    }

    let data = tempfile::tempdir().unwrap();
    let folder = data.path().join("crashes");
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            barrier.wait();
            assert_eq!(guarded(|| 1), Some(1));
        });
        scope.spawn(|| {
            barrier.wait();
            crate::crash::install(folder.clone());
        });
    });
    let _ = std::thread::spawn(|| panic!("after both")).join();
    assert!(folder.exists(), "the crash hook was lost");
    drop(std::panic::take_hook());
}
