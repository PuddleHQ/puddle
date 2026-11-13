// SPDX-License-Identifier: GPL-3.0-or-later
//! The runtime pin list, `ci/msb-runtime.sha256`: lines of `<tag> <name> <value>`. It is the one
//! source of what each fork tag's runtime is made of; `ci/fetch-msb.sh` reads the same lines
//! (`pinned`, reachable as `fetch-msb.sh --pinned <tag> <name>`), and a test keeps the two
//! parsers agreeing line by line.

use crate::error::{Result, XtaskError};

/// The workspace path of the pin list.
pub const PIN_FILE: &str = "ci/msb-runtime.sha256";

/// The pin naming the firmware version a tag's Linux msb looks for beside itself.
pub const LIBKRUNFW_LINUX_VERSION: &str = "libkrunfw-linux-version";

/// The pin naming the release of Puddle's libkrunfw fork ([`crate::source::FIRMWARE_FORK`]) a
/// tag's Linux firmware comes from, instead of upstream's.
pub const LIBKRUNFW_LINUX_TAG: &str = "libkrunfw-linux-tag";

// The reader is one file with `puddle-runtime`'s build script, which embeds the expected hashes
// from the same list.
#[path = "../../puddle-runtime/build/pin_list.rs"]
mod pin_list;

/// The value of the first line `<tag> <name> <value>` of `pins`: whitespace-separated fields, so
/// comment lines never match.
#[must_use]
pub fn pinned<'a>(pins: &'a str, tag: &str, name: &str) -> Option<&'a str> {
    pin_list::pinned(pins, tag, name)
}

/// The libkrunfw version `tag`'s Linux msb is built to look for (`libkrunfw.so.<version>`).
///
/// The value becomes part of a file name, so it must be a plain dotted number.
///
/// # Errors
///
/// [`XtaskError::Parse`] when the pin is missing or isn't digits separated by single dots.
pub fn libkrunfw_linux_version(pins: &str, tag: &str) -> Result<String> {
    let version = pinned(pins, tag, LIBKRUNFW_LINUX_VERSION).ok_or_else(|| {
        XtaskError::parse(
            PIN_FILE,
            format!("nothing pinned for {tag} {LIBKRUNFW_LINUX_VERSION}"),
        )
    })?;
    let plain = version
        .split('.')
        .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
    if !plain {
        return Err(XtaskError::parse(
            PIN_FILE,
            format!("{tag} {LIBKRUNFW_LINUX_VERSION} is {version:?}, not a dotted number"),
        ));
    }
    Ok(version.to_owned())
}

/// The libkrunfw fork release `tag`'s Linux firmware comes from, if the tag pins one.
#[must_use]
pub fn libkrunfw_linux_tag(pins: &str, tag: &str) -> Option<String> {
    pinned(pins, tag, LIBKRUNFW_LINUX_TAG).map(ToOwned::to_owned)
}

/// Checks `actual`, the SHA-256 of the downloaded `asset` of release `tag`, against the committed
/// pin list: a release's own `checksums.sha256` is replaceable together with its assets, this
/// list is reviewed in Puddle's repository.
///
/// # Errors
///
/// [`XtaskError::NotListed`] when the list has no line for `tag` and `asset`,
/// [`XtaskError::Checksum`] naming the file, the list and both hashes when it pins another hash.
pub fn verify(pins: &str, tag: &str, asset: &str, actual: &str) -> Result<()> {
    let list = format!("{PIN_FILE} ({tag})");
    let Some(expected) = pinned(pins, tag, asset) else {
        return Err(XtaskError::NotListed {
            file: asset.to_owned(),
            list,
        });
    };
    if expected.eq_ignore_ascii_case(actual) {
        return Ok(());
    }
    Err(XtaskError::Checksum {
        file: asset.to_owned(),
        list,
        expected: expected.to_ascii_lowercase(),
        actual: actual.to_owned(),
    })
}

#[cfg(test)]
mod tests;
