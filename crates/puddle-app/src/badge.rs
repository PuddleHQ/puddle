// SPDX-License-Identifier: AGPL-3.0-or-later
//! The badge: how many requests wait for the user, and how that shows on the tray icon, the
//! taskbar and the window title.

use std::collections::BTreeSet;

use puddle_appicon::Icon;
use puddle_types::Event;

/// The open requests, kept from the event stream so the badge needs no query per event.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenRequests {
    ids: BTreeSet<i64>,
}

impl OpenRequests {
    /// The requests that are open now (read from the store at start-up and after the event
    /// stream lost events).
    #[must_use]
    pub fn of(ids: impl IntoIterator<Item = i64>) -> Self {
        Self {
            ids: ids.into_iter().collect(),
        }
    }

    /// Follows an event. Returns whether the count changed.
    pub fn apply(&mut self, event: &Event) -> bool {
        match event {
            Event::PendingOpened { request } => self.ids.insert(request.id),
            Event::PendingClosed { id, .. } => self.ids.remove(id),
            _ => false,
        }
    }

    /// How many requests wait.
    #[must_use]
    pub fn count(&self) -> usize {
        self.ids.len()
    }
}

/// `3` as the title shows it; nothing waiting shows no number.
fn count_text(count: usize) -> String {
    if count > 99 {
        "99+".to_owned()
    } else {
        count.to_string()
    }
}

/// The window title: `Puddle (3)` while 3 requests wait and the badge is on, else `Puddle`.
#[must_use]
pub fn window_title(count: usize, on: bool) -> String {
    if on && count > 0 {
        format!("Puddle ({})", count_text(count))
    } else {
        "Puddle".to_owned()
    }
}

/// The tray icon's tooltip, and the line the tray menu shows above Open and Quit. This one is
/// always in words, with or without the badge, so the menu says what waits.
#[must_use]
pub fn waiting_line(count: usize) -> String {
    match count {
        0 => "Nothing is waiting".to_owned(),
        1 => "1 request is waiting".to_owned(),
        n => format!("{n} requests are waiting"),
    }
}

/// The tray tooltip: the app's name, and with the badge on, what waits.
#[must_use]
pub fn tooltip(count: usize, on: bool) -> String {
    if on && count > 0 {
        format!("Puddle: {}", waiting_line(count))
    } else {
        "Puddle".to_owned()
    }
}

/// Whether the tray icon's badge dot has a white ring round it. Off: the red dot is clear against
/// the duck on a light and a dark tray, and the overlay on the taskbar button has none either. Set
/// it to `true` to bring the ring back.
pub const TRAY_RING: bool = false;

/// The tray icon with the badge dot in its lower right corner. `None` when `base` is not a square
/// of four-byte pixels.
#[must_use]
pub fn with_dot(base: &Icon) -> Option<Icon> {
    puddle_appicon::badge_dot(base, TRAY_RING)
}

/// The Windows taskbar's overlay for the display `scale` (1.0 is 100 percent): the number of
/// waiting requests in white on a red dot, 16 pixels at 100 percent and 20, 24 and 32 at 125, 150
/// and 200, so Windows shows it as drawn. One to nine shows the digit, more shows `9+`. With
/// nothing waiting it is the plain dot (the shell shows no overlay then).
#[must_use]
pub fn overlay_number(count: usize, scale: f64) -> Option<Icon> {
    puddle_appicon::badge_overlay(count, puddle_appicon::overlay_side(scale))
}

#[cfg(test)]
mod tests;
