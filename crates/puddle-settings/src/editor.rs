// SPDX-License-Identifier: AGPL-3.0-or-later
//! The editor defaults of a workspace: colour theme, font size, preinstalled extensions and key
//! bindings. They layer like every other workspace setting (a workspace's value over the global
//! one over Puddle's default), field by field. The agent merges the resolved values into the
//! guest editor's own settings when the editor starts; see `Effective::editor`.

use std::collections::BTreeMap;

use puddle_types::ValidationError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The name of an editor colour theme, as the editor lists it (`Default Dark Modern`).
///
/// ```
/// use puddle_settings::ColourTheme;
/// assert_eq!(ColourTheme::new("  Solarized Dark ").unwrap().as_str(), "Solarized Dark");
/// assert!(ColourTheme::new("").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ColourTheme(String);

impl ColourTheme {
    /// Longest accepted name, in characters.
    pub const MAX_CHARS: usize = 100;

    /// Trims `name` and checks it: not empty, at most [`Self::MAX_CHARS`], no control characters.
    ///
    /// # Errors
    ///
    /// When the name breaks one of those rules.
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        let name = name.trim();
        let bad = if name.is_empty() {
            Some("must not be empty".to_owned())
        } else if name.chars().count() > Self::MAX_CHARS {
            Some(format!("must be at most {} characters", Self::MAX_CHARS))
        } else if name.chars().any(char::is_control) {
            Some("must not contain control characters".to_owned())
        } else {
            None
        };
        match bad {
            Some(reason) => Err(ValidationError::new("colour theme", name, reason)),
            None => Ok(Self(name.to_owned())),
        }
    }

    /// The name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ColourTheme {
    type Error = ValidationError;

    fn try_from(name: String) -> Result<Self, Self::Error> {
        Self::new(&name)
    }
}

impl From<ColourTheme> for String {
    fn from(t: ColourTheme) -> Self {
        t.0
    }
}

/// The editor's font size in pixels.
///
/// ```
/// use puddle_settings::EditorFontSize;
/// assert_eq!(EditorFontSize::new(14).unwrap().get(), 14);
/// assert!(EditorFontSize::new(7).is_err());
/// assert!(EditorFontSize::new(41).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct EditorFontSize(u8);

impl EditorFontSize {
    /// Smallest accepted size.
    pub const MIN: u8 = 8;
    /// Largest accepted size.
    pub const MAX: u8 = 40;

    /// Checks `px` and wraps it.
    ///
    /// # Errors
    ///
    /// When `px` is outside [`Self::MIN`]..=[`Self::MAX`].
    pub fn new(px: u8) -> Result<Self, ValidationError> {
        if (Self::MIN..=Self::MAX).contains(&px) {
            Ok(Self(px))
        } else {
            Err(ValidationError::new(
                "editor font size",
                &format!("{px}"),
                format_args!("must be between {} and {}", Self::MIN, Self::MAX),
            ))
        }
    }

    /// The size in pixels.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for EditorFontSize {
    type Error = ValidationError;

    fn try_from(px: u8) -> Result<Self, Self::Error> {
        Self::new(px)
    }
}

impl From<EditorFontSize> for u8 {
    fn from(s: EditorFontSize) -> Self {
        s.0
    }
}

/// An extension id in `publisher.name` form (`rust-lang.rust-analyzer`), as Open VSX and the
/// Visual Studio Marketplace spell it. Ids compare case-insensitively in the editor, so the
/// stored spelling is lower-cased.
///
/// ```
/// use puddle_settings::ExtensionId;
/// assert_eq!(ExtensionId::new("Rust-Lang.Rust-Analyzer").unwrap().as_str(), "rust-lang.rust-analyzer");
/// assert!(ExtensionId::new("nopublisher").is_err());
/// assert!(ExtensionId::new("a.b --install").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExtensionId(String);

impl ExtensionId {
    /// Longest accepted id, in characters.
    pub const MAX_CHARS: usize = 200;

    /// Checks `id` (one `.` splits publisher from name, each a run of letters, digits, `-`, `_`
    /// and, in the name, `.`) and lower-cases it.
    ///
    /// # Errors
    ///
    /// When the id is not of that form.
    pub fn new(id: &str) -> Result<Self, ValidationError> {
        let id = id.trim();
        let ok = id.chars().count() <= Self::MAX_CHARS
            && id.split_once('.').is_some_and(|(publisher, name)| {
                let word = |s: &str, dots: bool| {
                    s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
                        && s.chars().all(|c| {
                            c.is_ascii_alphanumeric() || c == '-' || c == '_' || (dots && c == '.')
                        })
                };
                word(publisher, false) && word(name, true)
            });
        if ok {
            Ok(Self(id.to_ascii_lowercase()))
        } else {
            Err(ValidationError::new(
                "extension id",
                id,
                "must look like publisher.name (letters, digits, '-', '_')",
            ))
        }
    }

    /// The id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ExtensionId {
    type Error = ValidationError;

    fn try_from(id: String) -> Result<Self, Self::Error> {
        Self::new(&id)
    }
}

impl From<ExtensionId> for String {
    fn from(id: ExtensionId) -> Self {
        id.0
    }
}

/// How many preinstalled extensions one level may list.
pub const MAX_EXTENSIONS: usize = 50;

/// The key bindings to merge into the editor: the text of a JSON array of
/// `{ "key": ..., "command": ... }` objects (an optional `when` and `args` ride along), the same
/// shape as the editor's `keybindings.json`.
///
/// ```
/// use puddle_settings::Keybindings;
/// let k = Keybindings::new(r#"[{"key":"ctrl+k ctrl+t","command":"workbench.action.selectTheme"}]"#).unwrap();
/// assert_eq!(k.entries().len(), 1);
/// assert!(Keybindings::new("{}").is_err());
/// assert!(Keybindings::new(r#"[{"key":"a"}]"#).is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Keybindings(String);

impl Keybindings {
    /// Largest accepted text, in bytes.
    pub const MAX_BYTES: usize = 64 * 1024;

    /// Checks `text`: within [`Self::MAX_BYTES`], valid JSON, an array whose entries are objects
    /// with a string `key` and a string `command` (a command may start with `-` to remove a
    /// default binding).
    ///
    /// # Errors
    ///
    /// When the text breaks one of those rules.
    pub fn new(text: &str) -> Result<Self, ValidationError> {
        let bad = |reason: &str| ValidationError::new("key bindings", text, reason);
        if text.len() > Self::MAX_BYTES {
            return Err(bad("must be at most 64 KiB"));
        }
        let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(text) else {
            return Err(bad("must be a JSON array of key binding objects"));
        };
        let field = |e: &Value, name: &str| {
            e.get(name)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.trim().is_empty())
        };
        if !entries
            .iter()
            .all(|e| field(e, "key") && field(e, "command"))
        {
            return Err(bad(
                "every entry needs a text \"key\" and a text \"command\"",
            ));
        }
        Ok(Self(text.to_owned()))
    }

    /// The text as stored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The parsed entries (the text was checked, so this is the array it holds).
    #[must_use]
    pub fn entries(&self) -> Vec<Value> {
        serde_json::from_str(&self.0).unwrap_or_default()
    }
}

impl TryFrom<String> for Keybindings {
    type Error = ValidationError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        Self::new(&text)
    }
}

impl From<Keybindings> for String {
    fn from(k: Keybindings) -> Self {
        k.0
    }
}

/// The editor defaults at one level, each optional (`None` = ask the next level).
///
/// ```
/// use puddle_settings::{ColourTheme, EditorLayer};
/// let mut layer = EditorLayer::default();
/// assert!(layer.is_empty());
/// layer.colour_theme = Some(ColourTheme::new("Default Dark Modern").unwrap());
/// assert!(!layer.is_empty());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EditorLayer {
    /// The colour theme. Unset follows the system colour scheme.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour_theme: Option<ColourTheme>,
    /// The font size in pixels. Unset keeps the editor's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<EditorFontSize>,
    /// Extensions to preinstall. A level that sets the list replaces the next level's list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Vec<ExtensionId>>,
    /// Key bindings to merge in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keybindings: Option<Keybindings>,
    #[serde(flatten)]
    pub(crate) extra: BTreeMap<String, Value>,
}

impl EditorLayer {
    /// Whether nothing is set at this level (unknown fields count as set, so they are kept).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn collect_unknown(&self, prefix: &str, out: &mut Vec<String>) {
        crate::document::push_unknown(prefix, &self.extra, out);
    }
}

/// What the editor starts with: the resolved defaults of one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct EffectiveEditor {
    /// The colour theme; `None` follows the system colour scheme.
    pub colour_theme: crate::Resolved<Option<ColourTheme>>,
    /// The font size; `None` keeps the editor's own.
    pub font_size: crate::Resolved<Option<EditorFontSize>>,
    /// The extensions to preinstall (none by default).
    pub extensions: crate::Resolved<Vec<ExtensionId>>,
    /// The key bindings to merge; `None` adds none.
    pub keybindings: crate::Resolved<Option<Keybindings>>,
}

impl EffectiveEditor {
    /// Puddle's defaults: follow the system scheme, the editor's own size, no extensions, no
    /// extra key bindings.
    pub const DEFAULTS: Self = Self {
        colour_theme: crate::resolve::default(None),
        font_size: crate::resolve::default(None),
        extensions: crate::resolve::default(Vec::new()),
        keybindings: crate::resolve::default(None),
    };

    /// `workspace` over `global` over [`Self::DEFAULTS`], field by field.
    #[must_use]
    pub fn resolve(global: &EditorLayer, workspace: &EditorLayer) -> Self {
        // Destructured so a new field doesn't compile until it is resolved here.
        let EditorLayer {
            colour_theme,
            font_size,
            extensions,
            keybindings,
            extra: _,
        } = workspace;
        Self {
            colour_theme: pick_with(
                colour_theme.as_ref(),
                global.colour_theme.as_ref(),
                |v| Some(v.clone()),
                Self::DEFAULTS.colour_theme,
            ),
            font_size: pick_with(
                font_size.as_ref(),
                global.font_size.as_ref(),
                |v| Some(*v),
                Self::DEFAULTS.font_size,
            ),
            extensions: pick_with(
                extensions.as_ref(),
                global.extensions.as_ref(),
                Clone::clone,
                Self::DEFAULTS.extensions,
            ),
            keybindings: pick_with(
                keybindings.as_ref(),
                global.keybindings.as_ref(),
                |v| Some(v.clone()),
                Self::DEFAULTS.keybindings,
            ),
        }
    }
}

fn pick_with<S, T>(
    workspace: Option<&S>,
    global: Option<&S>,
    lift: impl Fn(&S) -> T,
    builtin: crate::Resolved<T>,
) -> crate::Resolved<T> {
    use crate::Source;
    match (workspace, global) {
        (Some(v), _) => crate::Resolved {
            value: lift(v),
            source: Source::Workspace,
        },
        (None, Some(v)) => crate::Resolved {
            value: lift(v),
            source: Source::Global,
        },
        (None, None) => builtin,
    }
}

#[cfg(test)]
mod tests;
