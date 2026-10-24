// SPDX-License-Identifier: GPL-3.0-or-later
//! The tray icon and what the window's close and minimize buttons do.
//!
//! [`close_action`] and [`minimize_action`] are the matrix of the two Settings choices, as pure
//! functions. [`Tray`] is the icon, its menu (the number waiting, Open, Quit) and its badge.

use puddle_settings::{CloseBehaviour, UiPrefs};
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
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    let asked = || -> zbus::Result<bool> {
        let connection = zbus::blocking::Connection::session()?;
        let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
        let name = zbus::names::BusName::try_from(WATCHER)?;
        Ok(bus.name_has_owner(name)?)
    };
    asked().unwrap_or(false)
}

/// The tray icon.
pub struct Tray<R: Runtime> {
    icon: TrayIcon<R>,
    waiting: MenuItem<R>,
    plain: Image<'static>,
    marked: Option<Image<'static>>,
}

impl<R: Runtime> Tray<R> {
    /// Makes the tray icon with its menu and starts it showing `waiting` requests. `None` when
    /// the desktop has no tray (see [`TrayState::Missing`]): the reason is logged.
    pub fn build(app: &AppHandle<R>, waiting: usize, badge_on: bool) -> Option<Self> {
        if !can_show() {
            tracing::warn!(
                "this desktop shows no tray icons, so closing the window minimizes it instead"
            );
            return None;
        }
        match guarded(|| Self::make(app, waiting, badge_on)) {
            Some(Ok(tray)) => Some(tray),
            Some(Err(err)) => {
                tracing::warn!(%err, "no tray icon; closing the window minimizes it instead");
                None
            }
            None => {
                tracing::warn!(
                    "the tray icon library is missing; closing the window minimizes it instead"
                );
                None
            }
        }
    }

    fn make(app: &AppHandle<R>, waiting: usize, badge_on: bool) -> tauri::Result<Self> {
        let plain = app
            .default_window_icon()
            .cloned()
            .map(Image::to_owned)
            .ok_or_else(|| tauri::Error::AssetNotFound("the app icon".to_owned()))?;
        let marked = badge::with_dot(&Pixels {
            size: plain.width(),
            rgba: plain.rgba().to_vec(),
        })
        .filter(|p| p.size == plain.height())
        .map(|p| Image::new_owned(p.rgba, p.size, p.size));
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
            plain,
            marked,
        };
        tray.show(waiting, badge_on);
        Ok(tray)
    }

    /// Shows how many requests wait: the menu line always, the tooltip and the dot with the badge
    /// on.
    pub fn show(&self, waiting: usize, badge_on: bool) {
        let dotted = badge_on && waiting > 0;
        let image = match (&self.marked, dotted) {
            (Some(marked), true) => marked.clone(),
            _ => self.plain.clone(),
        };
        // A tray that is being torn down refuses these; there is nothing to do about it.
        let _ = self.waiting.set_text(badge::waiting_line(waiting));
        let _ = self
            .icon
            .set_tooltip(Some(badge::tooltip(waiting, badge_on)));
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
    fn a_panic_while_making_the_icon_is_an_answer_not_a_crash() {
        assert_eq!(guarded(|| 7), Some(7));
        let none: Option<u8> = guarded(|| panic!("no library"));
        assert_eq!(none, None);
    }
}
