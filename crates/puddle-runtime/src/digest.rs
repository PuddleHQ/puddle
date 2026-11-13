// SPDX-License-Identifier: GPL-3.0-or-later
//! The start-up check of the runtime files against the hashes this build of Puddle was made with.
//!
//! `build.rs` reads `ci/msb-runtime.sha256` and embeds the SHA-256 of the msb binary and the
//! firmware library of [`crate::BUILT_FOR`] for the target. [`crate::BundledRuntime::open`] hashes
//! the installed files and refuses any other bytes, so "runtime pinned exactly" holds for the
//! bytes and not only for the version string msb embeds. The file `runtime.json` in the install
//! folder is not consulted: whatever can replace msb there can replace it too.

use std::fmt::Write as _;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::platform::HostOs;
use crate::{DevOverride, RuntimeError, RuntimeLayout};

/// The expected hashes of the files in a runtime folder, as lowercase hex SHA-256. A file with no
/// expected hash isn't checked.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeDigests {
    msb: Option<String>,
    firmware: Option<String>,
    firmware_version: Option<String>,
}

impl RuntimeDigests {
    /// What this build was made for: the pin list's hashes for [`crate::BUILT_FOR`] on this target.
    /// Empty (nothing is checked) on a target the pin list doesn't cover.
    #[must_use]
    pub fn built_in() -> Self {
        let value = |text: &str| (!text.is_empty()).then(|| text.to_owned());
        Self {
            msb: value(env!("PUDDLE_MSB_SHA256")),
            firmware: value(env!("PUDDLE_FIRMWARE_SHA256")),
            firmware_version: value(env!("PUDDLE_FIRMWARE_VERSION")),
        }
    }

    /// No expected hashes: nothing is checked, and the status says so ([`DigestStatus::Unpinned`]).
    #[must_use]
    pub fn unpinned() -> Self {
        Self::default()
    }

    /// Expected hashes for msb and the firmware library under their names in the runtime folder
    /// ([`RuntimeLayout::libkrunfw_path`]), for tests and callers with their own list.
    #[must_use]
    pub fn pinned(msb: impl Into<String>, firmware: impl Into<String>) -> Self {
        Self {
            msb: Some(msb.into().to_ascii_lowercase()),
            firmware: Some(firmware.into().to_ascii_lowercase()),
            firmware_version: None,
        }
    }

    /// The files to hash and what each must hash to. The firmware is checked under every name
    /// that exists: on Linux that is the SONAME copy and the versioned one msb loads.
    fn checks(&self, layout: &RuntimeLayout) -> Vec<(PathBuf, &str)> {
        let mut checks = Vec::new();
        if let Some(expected) = &self.msb {
            checks.push((layout.msb_path(), expected.as_str()));
        }
        if let Some(expected) = &self.firmware {
            let mut paths = vec![layout.libkrunfw_path()];
            if let Some(name) = self
                .firmware_version
                .as_deref()
                .and_then(|version| HostOs::current().libkrunfw_versioned(version))
            {
                paths.push(layout.runtime_dir().join(name));
            }
            paths.dedup();
            checks.extend(
                paths
                    .into_iter()
                    .filter(|path| path.is_file())
                    .map(|path| (path, expected.as_str())),
            );
        }
        checks
    }
}

/// How the runtime files passed the hash check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestStatus {
    /// Every file with an expected hash has exactly that hash.
    Verified,
    /// No expected hash for this target, so nothing was checked.
    Unpinned,
    /// Some files differ, accepted only because the dev override is active; callers log this as
    /// a warning.
    Overridden {
        /// The files whose hash differs.
        files: Vec<PathBuf>,
    },
}

/// The lowercase hex SHA-256 of the file at `path`, read in one streaming pass.
///
/// # Errors
///
/// The error of opening or reading the file.
pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        // Writing to a String cannot fail.
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

/// Hashes the runtime files and compares them with `digests`.
///
/// # Errors
///
/// [`RuntimeError::Digest`] naming the file and both hashes for the first file that differs, unless
/// `dev` is active; [`RuntimeError::Unreadable`] when a file can't be read.
pub(crate) fn verify(
    layout: &RuntimeLayout,
    digests: &RuntimeDigests,
    dev: DevOverride,
) -> Result<DigestStatus, RuntimeError> {
    let checks = digests.checks(layout);
    if checks.is_empty() {
        return Ok(DigestStatus::Unpinned);
    }
    let mut overridden = Vec::new();
    for (path, expected) in checks {
        let found = sha256_file(&path).map_err(|e| RuntimeError::Unreadable {
            path: path.clone(),
            reason: e.to_string(),
        })?;
        if found == expected {
            continue;
        }
        if !dev.active() {
            return Err(RuntimeError::Digest {
                path,
                expected: expected.to_owned(),
                found,
            });
        }
        overridden.push(path);
    }
    Ok(if overridden.is_empty() {
        DigestStatus::Verified
    } else {
        DigestStatus::Overridden { files: overridden }
    })
}

#[cfg(test)]
mod tests;
