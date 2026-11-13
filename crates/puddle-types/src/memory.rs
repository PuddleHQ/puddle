// SPDX-License-Identifier: AGPL-3.0-or-later
//! Guest memory size. The setting itself (a global default with per-workspace overrides) lives in
//! `puddle-settings`.
//!
//! The value becomes msb's `--memory` at create; a change applies at the workspace's next start.
//! Puddle never sets `--max-memory`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// Guest memory in MiB, within [`MemoryMib::MIN`]..=[`MemoryMib::MAX`].
///
/// ```
/// use puddle_types::MemoryMib;
/// assert_eq!(MemoryMib::default(), MemoryMib::DEFAULT);
/// assert_eq!(MemoryMib::DEFAULT.get(), 8192);
/// assert!(MemoryMib::new(100).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct MemoryMib(u32);

impl MemoryMib {
    /// Smallest accepted size: 256 MiB.
    pub const MIN: MemoryMib = MemoryMib(256);
    /// Largest accepted size: 1 TiB.
    pub const MAX: MemoryMib = MemoryMib(1024 * 1024);
    /// The default for every workspace: 8 GiB.
    pub const DEFAULT: MemoryMib = MemoryMib(8 * 1024);

    /// Checks `mib` and wraps it.
    ///
    /// # Errors
    ///
    /// When `mib` is outside [`MemoryMib::MIN`]..=[`MemoryMib::MAX`].
    pub fn new(mib: u32) -> Result<Self, ValidationError> {
        if (Self::MIN.0..=Self::MAX.0).contains(&mib) {
            Ok(Self(mib))
        } else {
            Err(ValidationError::new(
                "memory size",
                &format!("{mib} MiB"),
                format_args!("must be between {} and {} MiB", Self::MIN.0, Self::MAX.0),
            ))
        }
    }

    /// The size in MiB.
    #[must_use]
    pub fn get(self) -> u32 {
        self.0
    }
}

impl Default for MemoryMib {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for MemoryMib {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} MiB", self.0)
    }
}

impl TryFrom<u32> for MemoryMib {
    type Error = ValidationError;

    fn try_from(mib: u32) -> Result<Self, Self::Error> {
        Self::new(mib)
    }
}

impl From<MemoryMib> for u32 {
    fn from(m: MemoryMib) -> u32 {
        m.0
    }
}

#[cfg(test)]
mod tests;
