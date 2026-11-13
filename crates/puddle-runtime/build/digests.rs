// SPDX-License-Identifier: GPL-3.0-or-later
//! Which hashes of the runtime files this build expects, shared by the build script and
//! `tests/digests.rs`: read from the pin list for the tag `v<BUILT_FOR>` and the target.

use crate::pin_list::pinned;

/// The pin naming the release of Puddle's libkrunfw fork a tag's Linux firmware comes from.
const FIRMWARE_TAG_PIN: &str = "libkrunfw-linux-tag";

/// The pin naming the firmware version a tag's Linux msb looks for beside itself.
const FIRMWARE_VERSION_PIN: &str = "libkrunfw-linux-version";

/// The expected SHA-256 (lowercase hex) of the files of one target; `None` where the pin list has
/// nothing for it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Expected {
    pub(crate) msb: Option<String>,
    pub(crate) firmware: Option<String>,
    /// The version in the Linux name msb loads the firmware under (`libkrunfw.so.<version>`).
    pub(crate) firmware_version: Option<String>,
}

/// The expected hashes for runtime `built_for` (`<release>-puddle.N`) on `os` (a
/// `CARGO_CFG_TARGET_OS` value) and `arch` (`CARGO_CFG_TARGET_ARCH`). Only the targets the pin
/// list covers (Windows and Linux on `x86_64`) have any.
///
/// # Errors
///
/// A pinned value that isn't 64 hex digits, or a firmware version that isn't a dotted number.
pub(crate) fn expected(
    pins: &str,
    built_for: &str,
    os: &str,
    arch: &str,
) -> Result<Expected, String> {
    let tag = format!("v{built_for}");
    let (msb, firmware_tag, firmware) = match (os, arch) {
        ("windows", "x86_64") => (
            "msb-windows-x86_64.exe",
            tag.clone(),
            "libkrunfw-windows-x86_64.dll",
        ),
        ("linux", "x86_64") => (
            "msb-linux-x86_64",
            pinned(pins, &tag, FIRMWARE_TAG_PIN).map_or_else(|| tag.clone(), str::to_owned),
            "libkrunfw-linux-x86_64.so",
        ),
        _ => return Ok(Expected::default()),
    };
    let hash = |tag: &str, name: &str| {
        pinned(pins, tag, name)
            .map(|value| {
                let valid = value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit());
                if valid {
                    Ok(value.to_ascii_lowercase())
                } else {
                    Err(format!(
                        "ci/msb-runtime.sha256: {tag} {name} is {value:?}, not a SHA-256"
                    ))
                }
            })
            .transpose()
    };
    let firmware_version = if os == "linux" {
        pinned(pins, &tag, FIRMWARE_VERSION_PIN)
            .map(|version| {
                let plain = version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
                if plain {
                    Ok(version.to_owned())
                } else {
                    Err(format!(
                        "ci/msb-runtime.sha256: {tag} {FIRMWARE_VERSION_PIN} is {version:?}, not a dotted number"
                    ))
                }
            })
            .transpose()?
    } else {
        None
    };
    Ok(Expected {
        msb: hash(&tag, msb)?,
        firmware: hash(&firmware_tag, firmware)?,
        firmware_version,
    })
}
