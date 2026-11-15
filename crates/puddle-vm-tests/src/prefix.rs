// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-run prefix that keeps parallel runs on one host apart.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use puddle_types::SandboxName;

use crate::error::{HarnessError, clip};

/// A short name every sandbox and directory of one run starts with.
///
/// Lowercase letters, digits and `-`, starting with a letter, at most [`RunPrefix::MAX_LEN`]
/// characters, so `<prefix>-<tag>` stays a valid Puddle sandbox name (a DNS label) and the private
/// home path stays short (msb's Unix sockets live under it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPrefix(String);

impl RunPrefix {
    /// Longest accepted prefix.
    pub const MAX_LEN: usize = 20;

    /// Validates a prefix given by the caller (CI passes `r<run id>-<attempt>`).
    ///
    /// # Errors
    ///
    /// [`HarnessError::InvalidPrefix`] when the value breaks the rules above.
    pub fn new(value: &str) -> Result<Self, HarnessError> {
        let starts_with_letter = value.chars().next().is_some_and(|c| c.is_ascii_lowercase());
        let charset_ok = value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if value.len() > Self::MAX_LEN || !starts_with_letter || !charset_ok || value.ends_with('-')
        {
            return Err(HarnessError::InvalidPrefix {
                value: clip(value),
                max: Self::MAX_LEN,
            });
        }
        Ok(Self(value.to_owned()))
    }

    /// Makes a prefix from the clock and the process id: `v` plus 8 base-36 digits.
    #[must_use]
    pub fn generate() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        Self::from_seed(nanos ^ (u128::from(std::process::id()) << 64))
    }

    fn from_seed(seed: u128) -> Self {
        const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let mut out = String::from("v");
        let mut rest = seed;
        for _ in 0..8 {
            let digit = usize::try_from(rest % 36).unwrap_or(0);
            out.push(char::from(DIGITS.get(digit).copied().unwrap_or(b'0')));
            rest /= 36;
        }
        Self(out)
    }

    /// Uses the given value when present, otherwise generates one.
    ///
    /// # Errors
    ///
    /// [`HarnessError::InvalidPrefix`] when a given value is invalid.
    pub fn from_value_or_generate(value: Option<&str>) -> Result<Self, HarnessError> {
        value.map_or_else(|| Ok(Self::generate()), Self::new)
    }

    /// The sandbox name `<prefix>-<tag>`.
    ///
    /// # Errors
    ///
    /// [`HarnessError::InvalidName`] when the result isn't a valid Puddle sandbox name.
    pub fn sandbox_name(&self, tag: &str) -> Result<SandboxName, HarnessError> {
        SandboxName::new(&format!("{}-{tag}", self.0)).map_err(|e| HarnessError::InvalidName {
            tag: clip(tag),
            reason: e.to_string(),
        })
    }

    /// Whether a sandbox name belongs to this run.
    #[must_use]
    pub fn owns(&self, name: &str) -> bool {
        name.strip_prefix(self.0.as_str())
            .is_some_and(|rest| rest.starts_with('-'))
    }

    /// The prefix text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RunPrefix {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests;
