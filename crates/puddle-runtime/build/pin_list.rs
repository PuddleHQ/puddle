// SPDX-License-Identifier: GPL-3.0-or-later
//! The reader of the runtime pin list, `ci/msb-runtime.sha256` (lines of `<tag> <name> <value>`).
//! One file, included by `puddle-runtime`'s build script and by `crates/xtask`, so the build that
//! embeds the expected hashes and the tool that installs the runtime read the list the same way.

/// The value of the first line `<tag> <name> <value>`: whitespace-separated fields, so comment
/// lines (first field `#`) never match.
#[must_use]
pub(crate) fn pinned<'a>(pins: &'a str, tag: &str, name: &str) -> Option<&'a str> {
    pins.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        match (fields.next(), fields.next(), fields.next()) {
            (Some(t), Some(n), Some(value)) if t == tag && n == name => Some(value),
            _ => None,
        }
    })
}
