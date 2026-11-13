// SPDX-License-Identifier: AGPL-3.0-or-later
//! Value types for single settings. Like `puddle-types`, each validates in its constructor and
//! when deserialised.

use std::fmt;
use std::time::Duration;

use puddle_types::ValidationError;
use serde::{Deserialize, Serialize};

/// How long browser VS Code keeps a session's extension host after its window disconnected
/// (`--reconnection-grace-time`), in whole seconds.
///
/// ```
/// use puddle_settings::ReconnectionGrace;
/// assert_eq!(ReconnectionGrace::default().secs(), 300);
/// assert!(ReconnectionGrace::new(5).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct ReconnectionGrace(u32);

impl ReconnectionGrace {
    /// Shortest accepted grace: 30 s, so a page reload or a short network drop survives.
    pub const MIN: ReconnectionGrace = ReconnectionGrace(30);
    /// Longest accepted grace: one day.
    pub const MAX: ReconnectionGrace = ReconnectionGrace(24 * 60 * 60);
    /// Puddle's default: 300 s.
    pub const DEFAULT: ReconnectionGrace = ReconnectionGrace(300);

    /// Checks `secs` and wraps it.
    ///
    /// # Errors
    ///
    /// When `secs` is outside [`ReconnectionGrace::MIN`]..=[`ReconnectionGrace::MAX`].
    pub fn new(secs: u32) -> Result<Self, ValidationError> {
        if (Self::MIN.0..=Self::MAX.0).contains(&secs) {
            Ok(Self(secs))
        } else {
            Err(ValidationError::new(
                "reconnection grace",
                &format!("{secs} s"),
                format_args!("must be between {} and {} s", Self::MIN.0, Self::MAX.0),
            ))
        }
    }

    /// The grace in seconds, as passed to the server.
    #[must_use]
    pub fn secs(self) -> u32 {
        self.0
    }

    /// The grace as a [`Duration`].
    #[must_use]
    pub fn duration(self) -> Duration {
        Duration::from_secs(u64::from(self.0))
    }
}

impl Default for ReconnectionGrace {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for ReconnectionGrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} s", self.0)
    }
}

impl TryFrom<u32> for ReconnectionGrace {
    type Error = ValidationError;

    fn try_from(secs: u32) -> Result<Self, Self::Error> {
        Self::new(secs)
    }
}

impl From<ReconnectionGrace> for u32 {
    fn from(g: ReconnectionGrace) -> u32 {
        g.0
    }
}

/// How full a workspace's disk (the root disk or the volume) may get, in percent, before Puddle
/// warns about it.
///
/// ```
/// use puddle_settings::DiskWarningPercent;
/// assert_eq!(DiskWarningPercent::default().get(), 95);
/// assert!(DiskWarningPercent::new(49).is_err());
/// assert!(DiskWarningPercent::new(101).is_err());
/// assert!(DiskWarningPercent::new(100).is_ok());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct DiskWarningPercent(u8);

impl DiskWarningPercent {
    /// Lowest accepted threshold: below half full a warning is noise.
    pub const MIN: DiskWarningPercent = DiskWarningPercent(50);
    /// Highest accepted threshold: warn only when the disk is full.
    pub const MAX: DiskWarningPercent = DiskWarningPercent(100);
    /// Puddle's default: 95 %.
    pub const DEFAULT: DiskWarningPercent = DiskWarningPercent(95);

    /// Checks `percent` and wraps it.
    ///
    /// # Errors
    ///
    /// When it is under [`Self::MIN`] or over [`Self::MAX`].
    pub fn new(percent: u8) -> Result<Self, ValidationError> {
        if (Self::MIN.0..=Self::MAX.0).contains(&percent) {
            Ok(Self(percent))
        } else {
            Err(ValidationError::new(
                "disk warning threshold",
                &format!("{percent} %"),
                format_args!("must be between {} and {} %", Self::MIN.0, Self::MAX.0),
            ))
        }
    }

    /// The threshold in percent.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }

    /// Whether `used` of `size` (any one unit) is at or over the threshold. A disk of no size
    /// is never full.
    #[must_use]
    pub fn reached(self, used: u64, size: u64) -> bool {
        size > 0 && u128::from(used) * 100 >= u128::from(size) * u128::from(self.0)
    }
}

impl Default for DiskWarningPercent {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u8> for DiskWarningPercent {
    type Error = ValidationError;

    fn try_from(percent: u8) -> Result<Self, Self::Error> {
        Self::new(percent)
    }
}

impl From<DiskWarningPercent> for u8 {
    fn from(p: DiskWarningPercent) -> u8 {
        p.0
    }
}

/// What happens when a page in a workspace window reads the clipboard programmatically.
///
/// The default asks once per workspace; the answer is stored as that workspace's override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardRead {
    /// Show Puddle's prompt.
    #[default]
    Ask,
    /// Allow without a prompt.
    Allow,
    /// Refuse without a prompt.
    Deny,
}

/// Which VS Code server browser VS Code runs. Microsoft's needs the user's consent
/// ([`crate::ConsentKind::VsCodeServer`]); the bundled code-server needs none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerChoice {
    /// The bundled code-server (extensions from Open VSX).
    #[default]
    CodeServer,
    /// Microsoft's VS Code server, downloaded from Microsoft after the user's consent.
    Microsoft,
}

/// The colour theme of Puddle's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

/// Which colour palette Puddle's window uses on top of the light or dark base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteChoice {
    /// Calm water blues with duck yellow for what needs the user.
    #[default]
    Pond,
    /// Violet blues, quiet and low in contrast noise.
    Dusk,
    /// Soft greens.
    Meadow,
    /// Warm reds and ambers.
    Ember,
    /// Neutral greys with a blue accent.
    Graphite,
    /// The user's own colours (see [`UiPrefs::custom_accent`](crate::UiPrefs)), Pond where unset.
    Custom,
}

/// A colour written as `#rrggbb`, kept in lower case.
///
/// ```
/// use puddle_settings::HexColour;
/// assert_eq!(HexColour::new("#0A6A85").unwrap().as_str(), "#0a6a85");
/// assert!(HexColour::new("teal").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HexColour(String);

impl HexColour {
    /// Checks that `text` is `#` and six hexadecimal digits, and keeps it in lower case.
    ///
    /// # Errors
    ///
    /// When `text` is anything else.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        let digits = text.strip_prefix('#').unwrap_or("");
        if digits.len() == 6 && digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            Ok(Self(text.to_ascii_lowercase()))
        } else {
            Err(ValidationError::new(
                "colour",
                text,
                format_args!("must be # and six hexadecimal digits, like #0a6a85"),
            ))
        }
    }

    /// The colour as `#rrggbb`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HexColour {
    type Error = ValidationError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(&text)
    }
}

impl From<HexColour> for String {
    fn from(c: HexColour) -> String {
        c.0
    }
}

/// How much room Puddle's window leaves around its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DensityChoice {
    /// The roomier layout.
    #[default]
    Comfortable,
    /// Tighter spacing, so more fits on screen.
    Compact,
}

/// The icon of Puddle's window, tray and taskbar button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppIconChoice {
    /// The duck on its patch of water, on a clear background.
    #[default]
    Duck,
    /// The duck on a pond-blue disc.
    DuckPond,
    /// The duck on a dark night-blue disc.
    DuckNight,
    /// A white duck on a sunny yellow disc.
    DuckSun,
    /// The image the user gave (kept per user, next to the settings).
    Custom,
}

/// What closing Puddle's window does while a workspace runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehaviour {
    /// Keep running in the tray, so workspaces keep working.
    #[default]
    Tray,
    /// Quit Puddle (and stop the workspaces).
    Quit,
}

#[cfg(test)]
mod tests;
