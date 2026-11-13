// SPDX-License-Identifier: AGPL-3.0-or-later
//! The colour of a rule set: a typed [`Fill`] that is one solid colour today.
//!
//! A fill is a tagged value (`{"kind": "solid", "color": "#0a6a85"}`), not a bare colour, so a
//! gradient can arrive later as one more kind. Records written before the kind existed, and every
//! stored `solid` fill, keep reading because the reader never changes shape for a kind it knows.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// One sRGB colour, written `#rrggbb` (lower case) on the wire and in storage.
///
/// ```
/// use puddle_types::Rgb;
/// let teal: Rgb = "#0A6A85".parse().unwrap();
/// assert_eq!(teal.to_string(), "#0a6a85");
/// assert!("teal".parse::<Rgb>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rgb(u8, u8, u8);

impl Rgb {
    /// A colour from its three channels.
    #[must_use]
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self(red, green, blue)
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

impl std::str::FromStr for Rgb {
    type Err = ValidationError;

    /// Accepts `#rrggbb` in either case; anything else (a name, a short form, an alpha channel)
    /// is refused so the stored value has one spelling.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bad = |reason: &str| ValidationError::new("colour", text, reason);
        let hex = text
            .strip_prefix('#')
            .ok_or_else(|| bad("must start with # and have six hex digits"))?;
        if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(bad("must be # and six hex digits, like #0a6a85"));
        }
        let channel = |at: usize| {
            hex.get(at..at + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| bad("must be # and six hex digits, like #0a6a85"))
        };
        Ok(Self(channel(0)?, channel(2)?, channel(4)?))
    }
}

impl Serialize for Rgb {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Rgb {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(feature = "openapi")]
mod schema {
    use utoipa::openapi::{ObjectBuilder, RefOr, Schema, Type};
    use utoipa::{PartialSchema, ToSchema};

    use super::Rgb;

    impl PartialSchema for Rgb {
        fn schema() -> RefOr<Schema> {
            ObjectBuilder::new()
                .schema_type(Type::String)
                .pattern(Some("^#[0-9a-fA-F]{6}$"))
                .examples(["#0a6a85"])
                .description(Some("A colour as `#rrggbb`."))
                .into()
        }
    }

    impl ToSchema for Rgb {}
}

/// What a rule set is painted with. Only `solid` exists today; a gradient would be a new kind
/// beside it, so a stored `solid` fill never needs a migration.
///
/// ```
/// use puddle_types::{Fill, Rgb};
/// let fill = Fill::solid(Rgb::new(10, 106, 133));
/// let json = serde_json::to_string(&fill).unwrap();
/// assert_eq!(json, r##"{"kind":"solid","color":"#0a6a85"}"##);
/// assert_eq!(serde_json::from_str::<Fill>(&json).unwrap(), fill);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        description = "What a rule set is painted with: one solid colour today. A gradient \
                          would be another `kind`."
    )
)]
#[non_exhaustive]
pub enum Fill {
    /// One colour.
    Solid {
        /// The colour.
        color: Rgb,
    },
}

impl Fill {
    /// One solid colour.
    #[must_use]
    pub const fn solid(color: Rgb) -> Self {
        Self::Solid { color }
    }

    /// The fill as the JSON the API and the store carry. Written by hand so it cannot fail; a
    /// test holds it to what serde writes.
    #[must_use]
    pub fn to_json(self) -> String {
        match self {
            Self::Solid { color } => format!(r#"{{"kind":"solid","color":"{color}"}}"#),
        }
    }

    /// The colour a single-colour view (a dot, a chip's text ink) is drawn from: the colour of a
    /// solid fill, and the first stop of a gradient when there is one.
    #[must_use]
    pub const fn primary(self) -> Rgb {
        match self {
            Self::Solid { color } => color,
        }
    }
}

/// The colours a set made without a choice is given, by its number. Every one has a readable dark
/// or light ink on it (the UI checks this) and they sit apart in hue.
pub const SET_PALETTE: [Rgb; 8] = [
    Rgb::new(0x2f, 0x80, 0xc9),
    Rgb::new(0x2e, 0xa0, 0x6e),
    Rgb::new(0xd9, 0x8a, 0x1f),
    Rgb::new(0xc0, 0x4f, 0x7a),
    Rgb::new(0x7a, 0x5b, 0xc7),
    Rgb::new(0x2a, 0xa1, 0xa8),
    Rgb::new(0xcf, 0x5b, 0x3c),
    Rgb::new(0x6f, 0x8f, 0x2e),
];

impl Fill {
    /// The default fill of the set you made with number `id`: the palette, round and round.
    #[must_use]
    pub fn default_for_user_set(id: i64) -> Self {
        let slot = usize::try_from(id.unsigned_abs() % SET_PALETTE.len() as u64).unwrap_or(0);
        Self::solid(SET_PALETTE.get(slot).copied().unwrap_or(SET_PALETTE[0]))
    }
}

#[cfg(test)]
mod tests;
