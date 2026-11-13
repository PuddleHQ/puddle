// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which icon the window, the tray and the taskbar button show: the built-in one the user chose,
//! or their own image, read from the settings folder.
//!
//! [`Wanted::read`] is cheap to repeat (it reads one small file at most), so the event loop asks
//! on every settings poll and only draws and sets an icon when the answer changed.

use puddle_api::SettingsRepo;
use puddle_appicon::{Icon, SIZE, built_in};
use puddle_settings::{AppIconChoice, UiPrefs};
use tauri::image::Image;

/// What the icon should be, before it is drawn: the choice, and the user's image when that is
/// the choice. Two equal values draw the same icon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Wanted {
    choice: AppIconChoice,
    /// The stored PNG, read only when `choice` is [`AppIconChoice::Custom`].
    custom: Option<Vec<u8>>,
}

impl Wanted {
    /// The icon `prefs` choose. A choice of the user's own image with no readable image behind it
    /// (deleted by hand, storage failing) is the duck, with a warning: the window still gets an
    /// icon.
    #[must_use]
    pub fn read(repo: &dyn SettingsRepo, prefs: &UiPrefs) -> Self {
        let choice = prefs.app_icon();
        if choice != AppIconChoice::Custom {
            return Self {
                choice,
                custom: None,
            };
        }
        match repo.load_app_icon() {
            Ok(Some(png)) => Self {
                choice,
                custom: Some(png),
            },
            Ok(None) => Self::fallback("there is no image stored"),
            Err(err) => Self::fallback(&err.to_string()),
        }
    }

    fn fallback(reason: &str) -> Self {
        tracing::warn!(%reason, "your own app icon cannot be used, so the duck is shown");
        Self {
            choice: AppIconChoice::Duck,
            custom: None,
        }
    }

    /// The icon, [`SIZE`] pixels square. `None` only when it cannot be drawn at all, which the
    /// caller treats as "keep the icon as it is".
    #[must_use]
    pub fn draw(&self) -> Option<Icon> {
        if let Some(png) = &self.custom {
            match Icon::from_png(png) {
                Ok(icon) => return Some(icon),
                Err(err) => {
                    tracing::warn!(%err, "your own app icon is damaged, so the duck is shown");
                }
            }
        }
        let choice = match self.choice {
            AppIconChoice::Custom => AppIconChoice::Duck,
            other => other,
        };
        built_in(choice, SIZE)
    }
}

/// `icon` as the image Tauri sets on a window or a tray.
#[must_use]
pub fn image(icon: &Icon) -> Image<'static> {
    Image::new_owned(icon.rgba.clone(), icon.size, icon.size)
}

#[cfg(test)]
mod tests;
