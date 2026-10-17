// SPDX-License-Identifier: GPL-3.0-or-later
//! Guest virtual CPU count. The setting itself (a global default with per-sandbox overrides)
//! lives in `puddle-settings`.
//!
//! The value becomes msb's `--cpus` at create; a change applies at the sandbox's next start.
//! Puddle never sets `--max-cpus` apart from the count itself (no CPU hotplug).

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValidationError;

/// Guest vCPUs, within [`CpuCount::MIN`]..=[`CpuCount::MAX`].
///
/// ```
/// use puddle_types::CpuCount;
/// assert_eq!(CpuCount::default(), CpuCount::DEFAULT);
/// assert_eq!(CpuCount::DEFAULT.get(), 1);
/// assert!(CpuCount::new(0).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct CpuCount(u8);

impl CpuCount {
    /// Smallest accepted count: one vCPU.
    pub const MIN: CpuCount = CpuCount(1);
    /// Largest accepted count.
    pub const MAX: CpuCount = CpuCount(32);
    /// The default for every sandbox: one vCPU, what msb boots without a count.
    pub const DEFAULT: CpuCount = CpuCount(1);

    /// Checks `count` and wraps it.
    ///
    /// # Errors
    ///
    /// When `count` is outside [`CpuCount::MIN`]..=[`CpuCount::MAX`].
    pub fn new(count: u32) -> Result<Self, ValidationError> {
        match u8::try_from(count) {
            Ok(n) if (Self::MIN.0..=Self::MAX.0).contains(&n) => Ok(Self(n)),
            _ => Err(ValidationError::new(
                "cpu count",
                &count.to_string(),
                format_args!("must be between {} and {}", Self::MIN.0, Self::MAX.0),
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
    fn from(c: CpuCount) -> u32 {
        u32::from(c.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_are_inclusive() {
        assert_eq!(CpuCount::new(1).unwrap(), CpuCount::MIN);
        assert_eq!(CpuCount::new(32).unwrap(), CpuCount::MAX);
        assert!(CpuCount::new(0).is_err());
        assert!(CpuCount::new(256).is_err());
        let err = CpuCount::new(33).unwrap_err();
        assert!(err.to_string().contains("between 1 and 32"), "{err}");
    }

    #[test]
    fn display_and_conversions() {
        assert_eq!(CpuCount::new(4).unwrap().to_string(), "4");
        assert_eq!(u32::from(CpuCount::MAX), 32);
        assert_eq!(CpuCount::try_from(2).unwrap().get(), 2);
        assert_eq!(CpuCount::default(), CpuCount::DEFAULT);
    }

    #[test]
    fn serde_validates_on_the_way_in() {
        assert_eq!(serde_json::to_string(&CpuCount::DEFAULT).unwrap(), "1");
        assert_eq!(serde_json::from_str::<CpuCount>("4").unwrap().get(), 4);
        assert!(serde_json::from_str::<CpuCount>("0").is_err());
        assert!(serde_json::from_str::<CpuCount>("300").is_err());
    }
}
