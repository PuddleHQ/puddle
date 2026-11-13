// SPDX-License-Identifier: AGPL-3.0-or-later
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
        if let Err(err) = self.icon.set_icon(Some(image)) {
            tracing::warn!(%err, "the tray did not take the app icon");
        }
    }
}

/// Runs `make`, returning `None` if it panicked (the Linux library that draws the icon panics
/// when it is not installed) without that panic writing a crash file: it is an answer, not a crash.
fn guarded<T>(make: impl FnOnce() -> T) -> Option<T> {
    puddle_types::panic_hook::silenced(make)
}

#[cfg(test)]
mod tests;
