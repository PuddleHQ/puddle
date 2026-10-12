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

/// The value of the first line `<tag> <name> <value>`: whitespace-separated fields, so comment
/// lines (first field `#`) never match.
#[must_use]
pub fn pinned<'a>(pins: &'a str, tag: &str, name: &str) -> Option<&'a str> {
    pins.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next(), fields.next()) {
            (Some(t), Some(n), Some(value)) if t == tag && n == name => Some(value),
            _ => None,
        }
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    const PINS: &str = "# comment v1 libkrunfw-linux-version 9\n\
        v1 msb-windows-x86_64.exe aaaa\n\
        v1 libkrunfw-linux-version 5.6.1\n\
        v2\tlibkrunfw-linux-version   5.7.0  \n\
        v3 libkrunfw-linux-version 5.7/../x\n\
        v4 libkrunfw-linux-version\n\
        v5 libkrunfw-linux-version 5..1\n";

    #[test]
    fn reads_the_value_of_the_matching_line() {
        assert_eq!(pinned(PINS, "v1", "msb-windows-x86_64.exe"), Some("aaaa"));
        assert_eq!(pinned(PINS, "v2", LIBKRUNFW_LINUX_VERSION), Some("5.7.0"));
        assert_eq!(pinned(PINS, "v9", LIBKRUNFW_LINUX_VERSION), None);
        assert_eq!(pinned(PINS, "v1", LIBKRUNFW_LINUX_VERSION), Some("5.6.1"));
        assert_eq!(pinned(PINS, "v4", LIBKRUNFW_LINUX_VERSION), None);
    }

    #[test]
    fn a_pin_bump_changes_the_version() {
        assert_eq!(libkrunfw_linux_version(PINS, "v1").unwrap(), "5.6.1");
        assert_eq!(libkrunfw_linux_version(PINS, "v2").unwrap(), "5.7.0");
    }

    #[test]
    fn a_missing_or_unsafe_version_is_refused() {
        for tag in ["v9", "v3", "v4", "v5"] {
            let err = libkrunfw_linux_version(PINS, tag).unwrap_err().to_string();
            assert!(err.contains(PIN_FILE) && err.contains(tag), "{err}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn agrees_with_fetch_msb_on_every_pin_line() {
        let root = crate::workspace_root();
        let pins = std::fs::read_to_string(root.join(PIN_FILE)).unwrap();
        let script = root.join("ci/fetch-msb.sh");
        let shell = |tag: &str, name: &str| {
            let out = std::process::Command::new("sh")
                .arg(&script)
                .args(["--pinned", tag, name])
                .output()
                .unwrap();
            out.status
                .success()
                .then(|| String::from_utf8(out.stdout).unwrap().trim_end().to_owned())
        };
        let mut seen = 0;
        let pairs = pins.lines().filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next().filter(|t| *t != "#")?, fields.next()?))
        });
        for (tag, name) in pairs {
            assert_eq!(
                shell(tag, name).as_deref(),
                pinned(&pins, tag, name),
                "{tag} {name}"
            );
            seen += 1;
        }
        assert!(seen > 10, "the pin list has {seen} lines?");
        assert_eq!(shell("v0.0.0-nope", LIBKRUNFW_LINUX_VERSION), None);
        assert_eq!(pinned(&pins, "v0.0.0-nope", LIBKRUNFW_LINUX_VERSION), None);
    }
}
