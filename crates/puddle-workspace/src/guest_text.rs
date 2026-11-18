// SPDX-License-Identifier: AGPL-3.0-or-later
//! Text a guest wrote, made safe to keep and show.

/// `text` cut at `max` characters with every control character (escape sequences, newlines, tabs)
/// replaced by `?`: the guest is untrusted, and what it prints reaches a terminal or a UI.
pub(crate) fn clean(text: &str, max: usize) -> String {
    text.chars()
        .take(max)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}
