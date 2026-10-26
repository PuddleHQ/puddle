// SPDX-License-Identifier: GPL-3.0-or-later
//! The app icon on the wire: which icon, the built-in ones to preview, and the user's own image.
//! Every field is always present (ADR 0002).

use puddle_settings as settings;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Which icon the window, the tray and the taskbar show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AppIconChoice {
    /// The duck on its patch of water (the default).
    Duck,
    /// The duck on a pond-blue disc.
    DuckPond,
    /// The duck on a dark night-blue disc.
    DuckNight,
    /// A white duck on a sunny yellow disc.
    DuckSun,
    /// The image the user gave; it must have been uploaded first.
    Custom,
}

impl From<settings::AppIconChoice> for AppIconChoice {
    fn from(c: settings::AppIconChoice) -> Self {
        match c {
            settings::AppIconChoice::Duck => Self::Duck,
            settings::AppIconChoice::DuckPond => Self::DuckPond,
            settings::AppIconChoice::DuckNight => Self::DuckNight,
            settings::AppIconChoice::DuckSun => Self::DuckSun,
            settings::AppIconChoice::Custom => Self::Custom,
        }
    }
}

impl From<AppIconChoice> for settings::AppIconChoice {
    fn from(c: AppIconChoice) -> Self {
        match c {
            AppIconChoice::Duck => Self::Duck,
            AppIconChoice::DuckPond => Self::DuckPond,
            AppIconChoice::DuckNight => Self::DuckNight,
            AppIconChoice::DuckSun => Self::DuckSun,
            AppIconChoice::Custom => Self::Custom,
        }
    }
}

/// One built-in icon, drawn for the Settings screen to preview.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BuiltInIcon {
    /// The choice that selects it.
    pub choice: AppIconChoice,
    /// A 64 pixel PNG of the icon, base64 (standard alphabet, padded).
    pub png_base64: String,
}

/// The icons the user can choose between.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AppIcons {
    /// The built-in icons, in the order to show them.
    pub built_in: Vec<BuiltInIcon>,
    /// The user's own image as it will be used (a 256 pixel PNG, base64); `null` until one is
    /// uploaded.
    #[schema(required = true)]
    pub custom_png_base64: Option<String>,
}

/// The user's own image after it was checked and resized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CustomIcon {
    /// The stored icon: a 256 pixel PNG, base64.
    pub png_base64: String,
}
