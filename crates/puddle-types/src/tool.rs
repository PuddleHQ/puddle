// SPDX-License-Identifier: AGPL-3.0-or-later
//! The values the Tools API sends to mise in the guest: a tool's name, a version, and the hash of
//! the file the user was looking at. All three are checked here, before they reach the guest, so
//! what mise gets is plain text it cannot read as an option, a path or a TOML key of its own.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// A tool's name in `mise.toml`: `node`, `npm:prettier`, `github:cli/cli`. ASCII letters, digits
/// and `. _ - : / @`, starting with a letter or digit, at most [`ToolName::MAX_LEN`] characters.
/// No quote, backslash, space or control character, so the name is safe inside the quoted TOML
/// key Puddle writes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        value_type = String,
        min_length = 1,
        max_length = 100,
        pattern = "^[A-Za-z0-9][A-Za-z0-9._:/@-]*$",
        example = "node",
        description = "A tool's name as mise.toml spells it: ASCII letters, digits and `.`, `_`, `-`, `:`, `/`, `@`, starting with a letter or digit."
    )
)]
pub struct ToolName(String);

impl ToolName {
    /// Longest name.
    pub const MAX_LEN: usize = 100;

    /// Checks `name` and wraps it.
    ///
    /// # Errors
    ///
    /// When `name` breaks one of the rules above.
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        check("tool name", name, Self::MAX_LEN, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@')
        })
        .map(|()| Self(name.to_owned()))
    }
}

/// The version a tool is requested at: `22`, `22.11.0`, `lts`, `latest`, `lts/iron`, `prefix:1.2`,
/// `sub-1:latest`. The same characters as a [`ToolName`]; it never starts with `-`, so it cannot be
/// read as an option, and no more than [`ToolVersion::MAX_LEN`] characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        value_type = String,
        min_length = 1,
        max_length = 100,
        pattern = "^[A-Za-z0-9][A-Za-z0-9._:/@+-]*$",
        example = "22",
        description = "A version request as mise reads it: ASCII letters, digits and `.`, `_`, `-`, `:`, `/`, `@`, `+`, starting with a letter or digit."
    )
)]
pub struct ToolVersion(String);

impl ToolVersion {
    /// Longest version.
    pub const MAX_LEN: usize = 100;

    /// Checks `version` and wraps it.
    ///
    /// # Errors
    ///
    /// When `version` breaks one of the rules above.
    pub fn new(version: &str) -> Result<Self, ValidationError> {
        check("tool version", version, Self::MAX_LEN, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@' | '+')
        })
        .map(|()| Self(version.to_owned()))
    }
}

/// What the hash of a file that is not there is called (`mise.toml` does not exist yet).
const ABSENT: &str = "absent";

/// The SHA-256 of the root `mise.toml` as the guest read it (64 lower-case hex digits), or
/// `absent` when the file does not exist. The UI sends it back with a change; a different hash
/// means the file changed since the user looked.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(
        value_type = String,
        pattern = "^([0-9a-f]{64}|absent)$",
        description = "The SHA-256 of the root mise.toml (64 lower-case hex digits), or `absent` when there is no such file."
    )
)]
pub struct ToolsFileHash(String);

impl ToolsFileHash {
    /// The hash of a file that does not exist.
    #[must_use]
    pub fn absent() -> Self {
        Self(ABSENT.to_owned())
    }

    /// Checks `hash` and wraps it.
    ///
    /// # Errors
    ///
    /// When `hash` is neither `absent` nor 64 lower-case hex digits.
    pub fn new(hash: &str) -> Result<Self, ValidationError> {
        let hex = hash.len() == 64 && hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if hex || hash == ABSENT {
            Ok(Self(hash.to_owned()))
        } else {
            Err(ValidationError::new(
                "file hash",
                hash,
                "must be 64 lower-case hex digits or `absent`",
            ))
        }
    }
}

fn check(
    what: &'static str,
    value: &str,
    max: usize,
    allowed: impl Fn(char) -> bool,
) -> Result<(), ValidationError> {
    let reason = if value.is_empty() {
        "must not be empty"
    } else if value.len() > max {
        "is too long"
    } else if !value.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        "must start with a letter or digit"
    } else if !value.chars().all(allowed) {
        "has a character that is not allowed"
    } else {
        return Ok(());
    };
    Err(ValidationError::new(what, value, reason))
}

macro_rules! plain_string {
    ($($ty:ident),*) => {$(
        impl $ty {
            /// The value as a string slice.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl FromStr for $ty {
            type Err = ValidationError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Self::new(s)
            }
        }

        impl TryFrom<String> for $ty {
            type Error = ValidationError;

            fn try_from(s: String) -> Result<Self, Self::Error> {
                Self::new(&s)
            }
        }

        impl From<$ty> for String {
            fn from(v: $ty) -> String {
                v.0
            }
        }
    )*};
}

plain_string!(ToolName, ToolVersion, ToolsFileHash);

#[cfg(test)]
mod tests;
