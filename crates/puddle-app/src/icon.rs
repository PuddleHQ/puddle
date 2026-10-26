// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use puddle_api::MemorySettings;

    use super::*;

    fn prefs(choice: Option<AppIconChoice>) -> UiPrefs {
        let mut prefs = UiPrefs::default();
        prefs.app_icon = choice;
        prefs
    }

    fn own_png() -> Vec<u8> {
        Icon {
            size: SIZE,
            rgba: [10, 20, 30, 255].repeat((SIZE * SIZE) as usize),
        }
        .to_png()
        .unwrap()
    }

    #[test]
    fn the_default_is_the_duck_and_a_built_in_reads_no_file() {
        let repo = MemorySettings::default();
        repo.save_app_icon(&own_png()).unwrap();
        let wanted = Wanted::read(&repo, &prefs(None));
        assert_eq!(wanted.choice, AppIconChoice::Duck);
        assert_eq!(wanted.custom, None, "the image is only read when chosen");
        assert_eq!(
            wanted.draw().unwrap(),
            built_in(AppIconChoice::Duck, SIZE).unwrap()
        );
        let night = Wanted::read(&repo, &prefs(Some(AppIconChoice::DuckNight)));
        assert_eq!(
            night.draw().unwrap(),
            built_in(AppIconChoice::DuckNight, SIZE).unwrap()
        );
        assert_ne!(night, wanted);
    }

    #[test]
    fn your_own_image_is_the_icon_and_a_replaced_image_is_a_different_wish() {
        let repo = MemorySettings::default();
        repo.save_app_icon(&own_png()).unwrap();
        let custom = prefs(Some(AppIconChoice::Custom));
        let first = Wanted::read(&repo, &custom);
        let icon = first.draw().unwrap();
        assert_eq!(icon.size, SIZE);
        assert_eq!(icon.rgba[..4], [10, 20, 30, 255]);
        assert_eq!(
            first,
            Wanted::read(&repo, &custom),
            "unchanged: nothing to redraw"
        );

        let other = Icon {
            size: SIZE,
            rgba: [1, 2, 3, 255].repeat((SIZE * SIZE) as usize),
        };
        repo.save_app_icon(&other.to_png().unwrap()).unwrap();
        let second = Wanted::read(&repo, &custom);
        assert_ne!(first, second);
        assert_eq!(second.draw().unwrap(), other);
    }

    #[test]
    fn a_missing_or_damaged_own_image_falls_back_to_the_duck() {
        let duck = built_in(AppIconChoice::Duck, SIZE).unwrap();
        let repo = MemorySettings::default();
        let custom = prefs(Some(AppIconChoice::Custom));
        assert_eq!(Wanted::read(&repo, &custom).draw().unwrap(), duck);
        repo.save_app_icon(b"not a png").unwrap();
        let damaged = Wanted::read(&repo, &custom);
        assert_eq!(damaged.draw().unwrap(), duck);
    }

    #[test]
    fn the_image_for_tauri_keeps_the_pixels() {
        let icon = built_in(AppIconChoice::DuckSun, 32).unwrap();
        let shown = image(&icon);
        assert_eq!((shown.width(), shown.height()), (32, 32));
        assert_eq!(shown.rgba(), icon.rgba);
    }
}
