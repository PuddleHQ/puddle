// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use super::*;

    #[test]
    fn grace_bounds_are_inclusive() {
        assert_eq!(ReconnectionGrace::new(30).unwrap(), ReconnectionGrace::MIN);
        assert_eq!(
            ReconnectionGrace::new(86_400).unwrap(),
            ReconnectionGrace::MAX
        );
        assert!(ReconnectionGrace::new(29).is_err());
        let err = ReconnectionGrace::new(86_401).unwrap_err();
        assert!(err.to_string().contains("between 30 and 86400 s"), "{err}");
    }

    #[test]
    fn grace_display_and_conversions() {
        let g = ReconnectionGrace::DEFAULT;
        assert_eq!(g.to_string(), "300 s");
        assert_eq!(g.duration(), Duration::from_secs(300));
        assert_eq!(u32::from(g), 300);
        assert_eq!(ReconnectionGrace::try_from(60).unwrap().secs(), 60);
    }

    #[test]
    fn grace_validates_when_deserialised() {
        assert_eq!(
            serde_json::from_str::<ReconnectionGrace>("120")
                .unwrap()
                .secs(),
            120
        );
        assert!(serde_json::from_str::<ReconnectionGrace>("0").is_err());
        assert_eq!(
            serde_json::to_string(&ReconnectionGrace::MIN).unwrap(),
            "30"
        );
    }

    #[test]
    fn clipboard_read_is_snake_case_and_defaults_to_ask() {
        assert_eq!(ClipboardRead::default(), ClipboardRead::Ask);
        assert_eq!(
            serde_json::to_string(&ClipboardRead::Allow).unwrap(),
            r#""allow""#
        );
        assert_eq!(
            serde_json::from_str::<ClipboardRead>(r#""deny""#).unwrap(),
            ClipboardRead::Deny
        );
        assert!(serde_json::from_str::<ClipboardRead>(r#""Deny""#).is_err());
    }

    #[test]
    fn choices_are_snake_case_and_default_to_the_safe_option() {
        assert_eq!(ServerChoice::default(), ServerChoice::CodeServer);
        assert_eq!(ThemeChoice::default(), ThemeChoice::System);
        assert_eq!(CloseBehaviour::default(), CloseBehaviour::Tray);
        assert_eq!(DensityChoice::default(), DensityChoice::Comfortable);
        assert_eq!(
            serde_json::from_str::<DensityChoice>(r#""compact""#).unwrap(),
            DensityChoice::Compact
        );
        assert!(serde_json::from_str::<DensityChoice>(r#""tight""#).is_err());
        assert_eq!(PaletteChoice::default(), PaletteChoice::Pond);
        assert_eq!(
            serde_json::from_str::<PaletteChoice>(r#""custom""#).unwrap(),
            PaletteChoice::Custom
        );
        assert!(serde_json::from_str::<PaletteChoice>(r#""neon""#).is_err());
        assert_eq!(
            serde_json::to_string(&ServerChoice::CodeServer).unwrap(),
            r#""code_server""#
        );
        assert_eq!(
            serde_json::from_str::<ServerChoice>(r#""microsoft""#).unwrap(),
            ServerChoice::Microsoft
        );
        assert_eq!(
            serde_json::from_str::<ThemeChoice>(r#""dark""#).unwrap(),
            ThemeChoice::Dark
        );
        assert_eq!(
            serde_json::from_str::<CloseBehaviour>(r#""quit""#).unwrap(),
            CloseBehaviour::Quit
        );
        assert!(serde_json::from_str::<ServerChoice>(r#""other""#).is_err());
    }

    #[test]
    fn colours_are_six_digit_hex_in_lower_case() {
        assert_eq!(HexColour::new("#0A6A85").unwrap().as_str(), "#0a6a85");
        for bad in [
            "", "#fff", "0a6a85", "#0a6a8", "#0a6a850", "#0a6a8g", "#+a6a85",
        ] {
            let err = HexColour::new(bad).unwrap_err();
            assert!(err.to_string().contains("six hexadecimal digits"), "{err}");
        }
        assert_eq!(
            serde_json::to_string(&HexColour::new("#ABCDEF").unwrap()).unwrap(),
            r##""#abcdef""##
        );
        assert!(serde_json::from_str::<HexColour>(r#""teal""#).is_err());
        assert_eq!(
            String::from(serde_json::from_str::<HexColour>(r##""#112233""##).unwrap()),
            "#112233"
        );
    }
}
