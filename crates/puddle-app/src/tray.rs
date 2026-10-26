// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray icon and what the window's close and minimize buttons do.
//!
//! [`close_action`] and [`minimize_action`] are the matrix of the two Settings choices, as pure
//! functions. [`Tray`] is the icon, its menu (the number waiting, Open, Quit) and its badge.

use puddle_settings::{CloseBehaviour, UiPrefs};
use std::sync::{Mutex, MutexGuard, PoisonError};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use tauri::{AppHandle, Runtime};

use crate::badge::{self, Pixels};

/// The menu entry that opens the window.
pub const MENU_OPEN: &str = "open";
/// The menu entry that quits Puddle (and, with it, stops every workspace). The application menu's
/// Ctrl+Q entry has the same id.
pub const MENU_QUIT: &str = "quit";

/// Whether there is a tray icon the user can see. Hiding the window is only safe with one: without
/// it nothing would say Puddle still runs, or bring the window back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayState {
    /// The icon is shown.
    Shown,
    /// There is none: the desktop has no place for tray icons, or the icon could not be made.
    Missing,
}

/// What the close button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseAction {
    /// Hide the window; Puddle keeps running in the tray.
    HideToTray,
    /// Minimize the window instead: the user chose the tray, but there is none.
    Minimize,
    /// Close the window and quit Puddle, which stops every workspace.
    Quit,
}

/// What minimizing does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimizeAction {
    /// What a window normally does: it goes to the taskbar.
    ToTaskbar,
    /// Hide the window in the tray.
    HideToTray,
}

/// What the close button does, from "Closing the window" in Settings and whether there is a tray.
#[must_use]
pub fn close_action(prefs: &UiPrefs, tray: TrayState) -> CloseAction {
    match (prefs.close_behaviour(), tray) {
        (CloseBehaviour::Quit, _) => CloseAction::Quit,
        (CloseBehaviour::Tray, TrayState::Shown) => CloseAction::HideToTray,
        (CloseBehaviour::Tray, TrayState::Missing) => CloseAction::Minimize,
    }
}

/// What minimizing does, from "Minimizing hides Puddle in the tray" and whether there is a tray.
#[must_use]
pub fn minimize_action(prefs: &UiPrefs, tray: TrayState) -> MinimizeAction {
    if prefs.minimize_to_tray() && tray == TrayState::Shown {
        MinimizeAction::HideToTray
    } else {
        MinimizeAction::ToTaskbar
    }
}

/// Whether the desktop has somewhere to show a tray icon. Windows and macOS always do. On Linux
/// the icon is a `StatusNotifierItem`, which needs a host on the session bus (GNOME has none
/// unless an extension is installed), and the library that talks to it.
#[cfg(not(target_os = "linux"))]
fn can_show() -> bool {
    true
}

/// Whether the session bus has a `StatusNotifier` host.
#[cfg(target_os = "linux")]
fn can_show() -> bool {
    // zbus 6 has no blocking API; its default runtime needs no tokio, so any `block_on` drives it.
    let watcher = async {
        let bus = zbus::Connection::session().await.ok()?;
        let name = zbus::names::BusName::try_from("org.kde.StatusNotifierWatcher").ok()?;
        zbus::fdo::DBusProxy::new(&bus)
            .await
            .ok()?
            .name_has_owner(name)
            .await
            .ok()
    };
    tauri::async_runtime::block_on(watcher).unwrap_or(false)
}

/// The app icon, and the same with the badge dot when it can be drawn on it.
fn icons(app_icon: &Image<'_>) -> (Image<'static>, Option<Image<'static>>) {
    let (size, rgba) = (app_icon.width(), app_icon.rgba().to_vec());
    let marked = badge::with_dot(&Pixels { size, rgba })
        .filter(|dotted| dotted.size == app_icon.height())
        .map(|dotted| Image::new_owned(dotted.rgba, dotted.size, dotted.size));
    (app_icon.clone().to_owned(), marked)
}

/// What the tray icon shows now: the app icon, with and without the badge dot, and how many
/// requests wait.
struct Look {
    plain: Image<'static>,
    marked: Option<Image<'static>>,
    waiting: usize,
    badge_on: bool,
}

impl Look {
    /// The image to show: the dotted one while requests wait and the badge is on.
    fn image(&self) -> Image<'static> {
        match (&self.marked, self.badge_on && self.waiting > 0) {
            (Some(marked), true) => marked.clone(),
            _ => self.plain.clone(),
        }
    }
}

/// The tray icon.
pub struct Tray<R: Runtime> {
    icon: TrayIcon<R>,
    waiting: MenuItem<R>,
    look: Mutex<Look>,
}

impl<R: Runtime> Tray<R> {
    /// Makes the tray icon from `app_icon`, with its menu, and starts it showing `waiting`
    /// requests. `None` when the desktop has no tray (see [`TrayState::Missing`]), for whatever
    /// reason: the log says.
    pub fn build(
        app: &AppHandle<R>,
        app_icon: &Image<'_>,
        waiting: usize,
        badge_on: bool,
    ) -> Option<Self> {
        let made = can_show().then(|| guarded(|| Self::make(app, app_icon, waiting, badge_on)));
        let tray = made.flatten().and_then(|made| {
            made.map_err(|err| tracing::debug!(%err, "the tray icon was refused"))
                .ok()
        });
        if tray.is_none() {
            tracing::warn!(
                "there is no tray icon here, so closing the window minimizes it instead"
            );
        }
        tray
    }

    fn make(
        app: &AppHandle<R>,
        app_icon: &Image<'_>,
        waiting: usize,
        badge_on: bool,
    ) -> tauri::Result<Self> {
        let (plain, marked) = icons(app_icon);
        let line = MenuItem::with_id(
            app,
            "waiting",
            badge::waiting_line(waiting),
            false,
            None::<&str>,
        )?;
        let open = MenuItem::with_id(app, MENU_OPEN, "Open Puddle", true, None::<&str>)?;
        let quit = MenuItem::with_id(app, MENU_QUIT, "Quit Puddle", true, None::<&str>)?;
        let menu = Menu::with_items(
            app,
            &[&line, &PredefinedMenuItem::separator(app)?, &open, &quit],
        )?;
        let icon = TrayIconBuilder::with_id("puddle")
            .icon(plain.clone())
            .tooltip(badge::tooltip(waiting, badge_on))
            .menu(&menu)
            .show_menu_on_left_click(false)
            .on_menu_event(|app, event| match event.id().as_ref() {
                MENU_OPEN => crate::shell::focus_main(app),
                MENU_QUIT => app.exit(0),
                _ => {}
            })
            .on_tray_icon_event(|tray, event| {
                if let TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                } = event
                {
                    crate::shell::focus_main(tray.app_handle());
                }
            })
            .build(app)?;
        let tray = Self {
            icon,
            waiting: line,
            look: Mutex::new(Look {
                plain,
                marked,
                waiting,
                badge_on,
            }),
        };
        tray.show(waiting, badge_on);
        Ok(tray)
    }

    fn look(&self) -> MutexGuard<'_, Look> {
        self.look.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Shows how many requests wait: the menu line always, the tooltip and the dot with the badge
    /// on.
    pub fn show(&self, waiting: usize, badge_on: bool) {
        let image = {
            let mut look = self.look();
            look.waiting = waiting;
            look.badge_on = badge_on;
            look.image()
        };
        // A tray that is being torn down refuses these; there is nothing to do about it.
        let _ = self.waiting.set_text(badge::waiting_line(waiting));
        let _ = self
            .icon
            .set_tooltip(Some(badge::tooltip(waiting, badge_on)));
        let _ = self.icon.set_icon(Some(image));
    }

    /// Changes the app icon the tray shows (the user chose another), keeping the count and the dot.
    pub fn set_app_icon(&self, app_icon: &Image<'_>) {
        let (plain, marked) = icons(app_icon);
        let image = {
            let mut look = self.look();
            look.plain = plain;
            look.marked = marked;
            look.image()
        };
        let _ = self.icon.set_icon(Some(image));
    }
}

/// Runs `make`, returning `None` if it panicked (the Linux library that draws the icon panics
/// when it is not installed) without that panic writing a crash file: it is an answer, not a crash.
fn guarded<T>(make: impl FnOnce() -> T) -> Option<T> {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(make));
    std::panic::set_hook(previous);
    result.ok()
}

#[cfg(test)]
mod tests {
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
}
