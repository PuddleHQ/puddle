// SPDX-License-Identifier: AGPL-3.0-or-later
//! Guest virtual CPU count. The setting itself (a global default with per-sandbox overrides)
//! lives in `puddle-settings`.
//!
//! The value becomes msb's `--cpus` at create; a change applies at the sandbox's next start.
//! Puddle never sets `--max-cpus` apart from the count itself (no CPU hotplug).
//!
//! What a user may choose is bounded by the computer's cores (a global maximum in
//! `puddle-settings`), not by this type: it only rejects zero and what a `u8` cannot hold.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// Guest vCPUs, at least [`CpuCount::MIN`].
///
/// ```
/// use puddle_types::CpuCount;
/// assert_eq!(CpuCount::default(), CpuCount::DEFAULT);
/// assert_eq!(CpuCount::DEFAULT.get(), 4);
/// assert!(CpuCount::new(0).is_err());
/// assert_eq!(CpuCount::default_for_host(2).get(), 1);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct CpuCount(u8);

impl CpuCount {
    /// Smallest accepted count: one vCPU.
    pub const MIN: Self = Self(1);
    /// Largest count the type holds. A storage width, not a policy: the limit a user meets is
    /// the global maximum, which defaults to the computer's logical cores.
    pub const MAX: Self = Self(u8::MAX);
    /// The default on a computer with at least eight logical cores: four vCPUs, what `JetBrains`
    /// Gateway needs. [`Self::default_for_host`] lowers it on a smaller computer.
    pub const DEFAULT: Self = Self(4);

    /// The default for a computer with `host_cores` logical cores: [`Self::DEFAULT`] but at most
    /// half the cores, and at least one.
    #[must_use]
    pub fn default_for_host(host_cores: u32) -> Self {
        let half = (host_cores / 2).max(u32::from(Self::MIN.0));
        Self(u8::try_from(half.min(u32::from(Self::DEFAULT.0))).unwrap_or(Self::DEFAULT.0))
    }

    /// `host_cores` as a count: at least one, at most [`Self::MAX`].
    #[must_use]
    pub fn clamped(host_cores: u32) -> Self {
        let n = host_cores.clamp(u32::from(Self::MIN.0), u32::from(Self::MAX.0));
        Self(u8::try_from(n).unwrap_or(Self::MAX.0))
    }

    /// Checks `count` and wraps it.
    ///
    /// # Errors
    ///
    /// When `count` is zero or above [`CpuCount::MAX`].
    pub fn new(count: u32) -> Result<Self, ValidationError> {
        match u8::try_from(count) {
            Ok(n) if (Self::MIN.0..=Self::MAX.0).contains(&n) => Ok(Self(n)),
            _ => Err(ValidationError::new(
                "cpu count",
                &count.to_string(),
                format_args!(
                    "must be at least {} and at most {}",
                    Self::MIN.0,
                    Self::MAX.0
                ),
            )),
        }
    }

    /// The count.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }
}

impl Default for CpuCount {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for CpuCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl TryFrom<u32> for CpuCount {
    type Error = ValidationError;

    fn try_from(count: u32) -> Result<Self, Self::Error> {
        Self::new(count)
    }
}

impl From<CpuCount> for u32 {
    fn from(c: CpuCount) -> Self {
        Self::from(c.0)
    }
}

#[cfg(test)]
mod tests;
