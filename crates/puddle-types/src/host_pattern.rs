// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host pattern of a rule that works on decrypted traffic: one name, or every name below a
//! suffix.

use std::fmt;

use crate::error::ValidationError;
use crate::host::DomainName;

const WHAT_PATTERN: &str = "host pattern";

/// A host pattern in stored form: `example.com` (that name only) or `*.example.com` (every name
/// below `example.com`, not `example.com` itself).
///
/// The name is already normalised (see [`DomainName`]) and a `*.` pattern has at least two labels
/// after it, so `*.com` is refused: a pattern that is held here is one the proxy takes, and a
/// value that is not one never reaches a rule that would then silently check nothing. An IP
/// literal is no pattern.
///
/// ```
/// use puddle_types::HostPattern;
/// assert_eq!(HostPattern::parse_normalised("*.example.com").unwrap().to_string(), "*.example.com");
/// assert!(HostPattern::parse_normalised("*.com").is_err());
/// assert!(HostPattern::parse_normalised("203.0.113.7").is_err());
/// assert!(HostPattern::parse_normalised("Example.com").is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HostPattern {
    name: DomainName,
    below: bool,
}

impl HostPattern {
    /// Accepts `text` only if it is already in stored form; never rewrites it.
    ///
    /// # Errors
    /// A [`ValidationError`] when the name is not a normalised name or a `*.` suffix has fewer
    /// than two labels.
    pub fn parse_normalised(text: &str) -> Result<Self, ValidationError> {
        let (below, rest) = match text.strip_prefix("*.") {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let name = DomainName::parse_normalised(rest)
            .map_err(|e| ValidationError::new(WHAT_PATTERN, text, e.to_string()))?;
        if below && name.labels().count() < 2 {
            return Err(ValidationError::new(
                WHAT_PATTERN,
                text,
                "a wildcard needs at least two labels after `*.`",
            ));
        }
        Ok(Self { name, below })
    }

    /// The name, without any `*.`.
    #[must_use]
    pub fn name(&self) -> &DomainName {
        &self.name
    }

    /// Whether this names every name below [`Self::name`] (a `*.` pattern) rather than that name.
    #[must_use]
    pub fn is_below(&self) -> bool {
        self.below
    }
}

impl fmt::Display for HostPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.below {
            f.write_str("*.")?;
        }
        f.write_str(self.name.as_str())
    }
}

#[cfg(test)]
mod tests;
