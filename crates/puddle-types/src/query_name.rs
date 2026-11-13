// SPDX-License-Identifier: AGPL-3.0-or-later
//! The name of a URL query parameter a secret's stand-in is swapped in.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// The longest [`QueryName`].
pub const MAX_QUERY_NAME_LEN: usize = 64;

/// The name of a query parameter: 1 to 64 visible ASCII characters, none of `&`, `=`, `#`, `%`
/// or `+`, so the name is the same text raw and percent-decoded and never splits a pair.
///
/// ```
/// use puddle_types::QueryName;
/// assert!(QueryName::new("appid").is_ok());
/// assert!(QueryName::new("a&b").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct QueryName(String);

impl QueryName {
    /// Checks `name` and wraps it.
    ///
    /// # Errors
    ///
    /// When `name` is empty, longer than [`MAX_QUERY_NAME_LEN`], or has a character that is not
    /// visible ASCII or is one of `&`, `=`, `#`, `%`, `+`.
    pub fn new(name: &str) -> Result<Self, ValidationError> {
        const WHAT: &str = "query parameter name";
        if name.is_empty() || name.len() > MAX_QUERY_NAME_LEN {
            return Err(ValidationError::new(
                WHAT,
                name,
                format_args!("must be 1 to {MAX_QUERY_NAME_LEN} characters"),
            ));
        }
        if !name
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'&' | b'=' | b'#' | b'%' | b'+'))
        {
            return Err(ValidationError::new(
                WHAT,
                name,
                "may only contain visible ASCII characters, none of & = # % +",
            ));
        }
        Ok(Self(name.to_owned()))
    }

    /// The name as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QueryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for QueryName {
    type Error = ValidationError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(&s)
    }
}

impl From<QueryName> for String {
    fn from(n: QueryName) -> Self {
        n.0
    }
}

#[cfg(test)]
mod tests;
