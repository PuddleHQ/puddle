// SPDX-License-Identifier: AGPL-3.0-or-later
//! How little free space on the drive that holds Puddle's data is a warning and how little is a
//! failure: the two lines `puddle doctor` and the app's notice both read.

use puddle_types::{DiskSpaceLevel, ValidationError};
use serde::{Deserialize, Serialize};

const GIB: u64 = 1024 * 1024 * 1024;

/// An amount of free disk space in whole GB of 1024 x 1024 x 1024 bytes (the unit Windows and most
/// Linux tools show as GB).
///
/// ```
/// use puddle_settings::FreeGb;
/// assert_eq!(FreeGb::new(20).unwrap().bytes(), 20 * 1024 * 1024 * 1024);
/// assert!(FreeGb::new(0).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u32", into = "u32")]
pub struct FreeGb(u32);

impl FreeGb {
    /// Smallest accepted line: 1 GB. Zero would never warn.
    pub const MIN: FreeGb = FreeGb(1);
    /// Largest accepted line: 10 000 GB.
    pub const MAX: FreeGb = FreeGb(10_000);
    /// Puddle's default for the warning line: 20 GB.
    pub const DEFAULT_WARN: FreeGb = FreeGb(20);
    /// Puddle's default for the failure line: 5 GB.
    pub const DEFAULT_FAIL: FreeGb = FreeGb(5);

    /// Checks `gb` and wraps it.
    ///
    /// # Errors
    ///
    /// When `gb` is outside [`FreeGb::MIN`]..=[`FreeGb::MAX`].
    pub fn new(gb: u32) -> Result<Self, ValidationError> {
        if (Self::MIN.0..=Self::MAX.0).contains(&gb) {
            Ok(Self(gb))
        } else {
            Err(ValidationError::new(
                "free disk space",
                &format!("{gb} GB"),
                format_args!("must be between {} and {} GB", Self::MIN.0, Self::MAX.0),
            ))
        }
    }

    /// The amount in GB.
    #[must_use]
    pub fn gb(self) -> u32 {
        self.0
    }

    /// The amount in bytes.
    #[must_use]
    pub fn bytes(self) -> u64 {
        u64::from(self.0) * GIB
    }
}

impl TryFrom<u32> for FreeGb {
    type Error = ValidationError;

    fn try_from(gb: u32) -> Result<Self, Self::Error> {
        Self::new(gb)
    }
}

impl From<FreeGb> for u32 {
    fn from(g: FreeGb) -> u32 {
        g.0
    }
}

/// The two lines together: the failure line is never above the warning line.
///
/// ```
/// use puddle_settings::{DiskSpaceLimits, FreeGb};
/// use puddle_types::DiskSpaceLevel;
/// let limits = DiskSpaceLimits::default();
/// let gb = |n: u64| n * 1024 * 1024 * 1024;
/// assert_eq!(limits.level(gb(30)), DiskSpaceLevel::Ok);
/// assert_eq!(limits.level(gb(20)), DiskSpaceLevel::Ok);
/// assert_eq!(limits.level(gb(20) - 1), DiskSpaceLevel::Low);
/// assert_eq!(limits.level(gb(5) - 1), DiskSpaceLevel::Critical);
/// assert!(DiskSpaceLimits::new(FreeGb::new(5).unwrap(), FreeGb::new(6).unwrap()).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DiskSpaceLimits {
    warn: FreeGb,
    fail: FreeGb,
}

impl DiskSpaceLimits {
    /// Checks that `fail` is not above `warn`.
    ///
    /// # Errors
    ///
    /// When it is: a failure line above the warning line would make the warning unreachable.
    pub fn new(warn: FreeGb, fail: FreeGb) -> Result<Self, ValidationError> {
        if fail > warn {
            return Err(ValidationError::new(
                "free disk space",
                &format!("{} GB", fail.gb()),
                format_args!(
                    "the failure line must not be above the warning line ({} GB)",
                    warn.gb()
                ),
            ));
        }
        Ok(Self { warn, fail })
    }

    /// Like [`DiskSpaceLimits::new`], but a failure line above the warning line is lowered to it
    /// instead of refused: for stored values nothing has validated together.
    #[must_use]
    pub fn clamped(warn: FreeGb, fail: FreeGb) -> Self {
        Self {
            warn,
            fail: fail.min(warn),
        }
    }

    /// The warning line.
    #[must_use]
    pub fn warn(self) -> FreeGb {
        self.warn
    }

    /// The failure line.
    #[must_use]
    pub fn fail(self) -> FreeGb {
        self.fail
    }

    /// Where `free_bytes` of free space stands: under the failure line is critical, under the
    /// warning line low, anything else fine.
    #[must_use]
    pub fn level(self, free_bytes: u64) -> DiskSpaceLevel {
        if free_bytes < self.fail.bytes() {
            DiskSpaceLevel::Critical
        } else if free_bytes < self.warn.bytes() {
            DiskSpaceLevel::Low
        } else {
            DiskSpaceLevel::Ok
        }
    }
}

impl Default for DiskSpaceLimits {
    fn default() -> Self {
        Self {
            warn: FreeGb::DEFAULT_WARN,
            fail: FreeGb::DEFAULT_FAIL,
        }
    }
}

#[cfg(test)]
mod tests;
