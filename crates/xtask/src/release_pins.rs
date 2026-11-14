// SPDX-License-Identifier: AGPL-3.0-or-later
//! The release-pin check: a release build refuses a runtime pin that has no hash.
//!
//! `puddle-runtime`'s build script embeds the SHA-256 of the msb binary and the firmware library
//! that `ci/msb-runtime.sha256` pins for the tag; a target with none gets
//! `DigestStatus::Unpinned`, which opens the runtime unchecked and says nothing. That is fine for
//! a developer build of an old tag, never for a release, so the release's `verify` job runs this
//! check for each OS it ships. It asks the build script's own reader ([`crate::digests`]), so
//! "no hash here" means the same in both places.

use crate::digests::{Expected, expected};
use crate::error::{Result, XtaskError};
use crate::pin::PIN_FILE;

/// Checks that the pin list gives the build of runtime `built_for` (`<release>-puddle.N`) a hash
/// for every file it must verify on each of `oses` (`windows` or `linux`, on `x86_64`).
///
/// # Errors
///
/// [`XtaskError::Unpinned`] naming each OS and file with no pin, [`XtaskError::Parse`] for a
/// pinned value that isn't a SHA-256.
pub fn check(pins: &str, built_for: &str, oses: &[&str]) -> Result<String> {
    let mut missing = Vec::new();
    for os in oses {
        let found =
            expected(pins, built_for, os, "x86_64").map_err(|e| XtaskError::parse(PIN_FILE, e))?;
        missing.extend(
            gaps(&found, os).map(|what| format!("{PIN_FILE}: v{built_for} has no {what} for {os}")),
        );
    }
    if !missing.is_empty() {
        return Err(XtaskError::Unpinned(missing.join("\n")));
    }
    Ok(format!(
        "release pins ok: v{built_for} pins the msb and firmware hashes for {}",
        oses.join(", ")
    ))
}

/// What a release for `os` needs that `found` lacks.
fn gaps<'a>(found: &'a Expected, os: &str) -> impl Iterator<Item = &'static str> + 'a {
    let linux = os == "linux";
    [
        (found.msb.is_none(), "msb hash"),
        (found.firmware.is_none(), "firmware hash"),
        (
            linux && found.firmware_version.is_none(),
            "firmware version",
        ),
    ]
    .into_iter()
    .filter_map(|(absent, what)| absent.then_some(what))
}

#[cfg(test)]
mod tests;
