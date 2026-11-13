// SPDX-License-Identifier: GPL-3.0-or-later
//! The pinned runtime version and the exact-match rule (refuse a runtime Puddle wasn't built
//! for; an override for dev builds only).

use std::fmt;
use std::str::FromStr;

/// The msb fork version this build of Puddle runs, as `<release>-puddle.<N>` (fork tag
/// `v<release>-puddle.N`). It is the Cargo package version the fork's `msb` embeds in its
/// `.msbver` section, so the fork sets its workspace version to this string when it tags.
///
/// Not written here: the build script takes it from the msb SDK's package version in the workspace
/// `Cargo.lock`, which comes from the fork tag in the root `Cargo.toml` (and fails the build if
/// the two disagree). So the SDK and the runtime it expects can't drift apart.
pub const BUILT_FOR: &str = env!("PUDDLE_MSB_BUILT_FOR");

/// The environment variable that, in builds with the `dev-override` feature, makes Puddle accept a
/// runtime of any version (value `1`).
pub const DEV_OVERRIDE_VAR: &str = "PUDDLE_DEV_ANY_RUNTIME";

/// Whether this build contains the dev override at all. `false` in every shipped build.
pub const DEV_OVERRIDE_COMPILED: bool = cfg!(feature = "dev-override");

/// A fork runtime version: an upstream msb release plus the fork's revision on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RuntimeVersion {
    major: u64,
    minor: u64,
    patch: u64,
    puddle: u32,
}

impl RuntimeVersion {
    /// The version this build was made for ([`BUILT_FOR`]), written by the build script as a
    /// constant: it is the parsed, checked number, so there is no parse to fail here.
    #[must_use]
    pub const fn built_for() -> Self {
        include!(concat!(env!("OUT_DIR"), "/built_for_version.rs"))
    }

    /// Only for the build script's generated constant, which writes the numbers its own check
    /// accepted.
    const fn from_parts(major: u64, minor: u64, patch: u64, puddle: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            puddle,
        }
    }

    /// The upstream msb release, `MAJOR.MINOR.PATCH` (the fork's base tag without the `v`).
    #[must_use]
    pub fn release(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }

    /// The fork revision `N` on that release (≥ 1).
    #[must_use]
    pub fn puddle_revision(&self) -> u32 {
        self.puddle
    }

    /// The fork's git tag for this version, `v<release>-puddle.N`.
    #[must_use]
    pub fn fork_tag(&self) -> String {
        format!("v{self}")
    }

    /// Upstream's git tag for the release, `v<release>`.
    #[must_use]
    pub fn upstream_tag(&self) -> String {
        format!("v{}", self.release())
    }
}

impl fmt::Display for RuntimeVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{}.{}-puddle.{}",
            self.major, self.minor, self.patch, self.puddle
        )
    }
}

/// Why a string isn't a `<release>-puddle.N` version.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a <release>-puddle.N runtime version: {value:?}")]
pub struct ParseVersionError {
    value: String,
}

impl FromStr for RuntimeVersion {
    type Err = ParseVersionError;

    /// Parses `MAJOR.MINOR.PATCH-puddle.N`: decimal numbers without leading zeros (semver), `N` ≥ 1,
    /// no build metadata, nothing else.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let fail = || ParseVersionError {
            value: printable(s),
        };
        let (release, puddle) = s.split_once("-puddle.").ok_or_else(fail)?;
        let mut parts = release.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(fail());
        };
        let puddle: u32 = number(puddle).ok_or_else(fail)?;
        if puddle == 0 {
            return Err(fail());
        }
        Ok(Self {
            major: number(major).ok_or_else(fail)?,
            minor: number(minor).ok_or_else(fail)?,
            patch: number(patch).ok_or_else(fail)?,
            puddle,
        })
    }
}

/// A semver numeric identifier: ASCII digits, no leading zero unless it is `0`.
fn number<T: FromStr>(s: &str) -> Option<T> {
    let digits = !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let leading_zero = s.len() > 1 && s.starts_with('0');
    if digits && !leading_zero {
        s.parse().ok()
    } else {
        None
    }
}

/// `s` made safe to print: control and non-ASCII characters escaped, cut to 64 characters.
pub(crate) fn printable(s: &str) -> String {
    let escaped: String = s.escape_default().collect();
    if escaped.chars().count() > 64 {
        let mut cut: String = escaped.chars().take(61).collect();
        cut.push_str("...");
        cut
    } else {
        escaped
    }
}

/// Whether the developer asked to accept any runtime version. Only builds with the `dev-override`
/// feature honour it ([`DEV_OVERRIDE_COMPILED`]); everywhere else it is ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DevOverride {
    requested: bool,
}

impl DevOverride {
    /// No override: the default, and what every shipped build behaves like.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// An explicit request (tests, or a caller with its own setting).
    #[must_use]
    pub fn requested() -> Self {
        Self { requested: true }
    }

    /// Reads [`DEV_OVERRIDE_VAR`] from the process environment: requested only when it is `1`.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            requested: std::env::var_os(DEV_OVERRIDE_VAR).is_some_and(|v| v == "1"),
        }
    }

    /// Whether the override is in force: requested *and* compiled in.
    #[must_use]
    pub fn active(self) -> bool {
        self.requested && DEV_OVERRIDE_COMPILED
    }
}

/// How the bundled runtime passed the version check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionStatus {
    /// Exactly the version this build was made for.
    Exact,
    /// Another version (or none), accepted only because the dev override is active. Callers log
    /// this as a warning.
    Overridden {
        /// What the binary says (`None`: no version section), printable, at most 64 characters.
        found: Option<String>,
    },
}

/// Why [`check_version`] refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionRefusal {
    /// The binary has no embedded version.
    NoVersion,
    /// The binary is another version; the printable found value.
    Mismatch(String),
}

/// The exact-match rule: `found` (the binary's embedded version, `None` when it has none) must be
/// exactly `expected`, byte for byte. A newer or older release, another fork revision, or plain
/// upstream `0.7.7` are all refused, unless `dev` is active.
///
/// # Errors
///
/// [`VersionRefusal`] when the versions differ and the override isn't active.
pub fn check_version(
    expected: &RuntimeVersion,
    found: Option<&str>,
    dev: DevOverride,
) -> Result<VersionStatus, VersionRefusal> {
    let exact = found.is_some_and(|f| f == expected.to_string());
    if exact {
        return Ok(VersionStatus::Exact);
    }
    if dev.active() {
        return Ok(VersionStatus::Overridden {
            found: found.map(printable),
        });
    }
    Err(match found {
        None => VersionRefusal::NoVersion,
        Some(f) => VersionRefusal::Mismatch(printable(f)),
    })
}

#[cfg(test)]
mod tests;
