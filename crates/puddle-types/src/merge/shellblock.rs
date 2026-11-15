// SPDX-License-Identifier: GPL-3.0-or-later
//! A shell startup file (`/etc/bash.bashrc`, `/etc/zsh/zshenv`): Puddle owns the lines between two
//! marker comments and nothing else.
//!
//! The file belongs to the image, so every byte outside the markers stays as it is. The engine is
//! the one `ssh_known_hosts` uses (`knownhosts`), with markers that are shell comments. The block
//! is shell text (`export NAME='value'` lines); any text is valid but a NUL byte and a line that
//! is one of the markers (a value could otherwise end the block early).

use super::knownhosts::{self, Markers};
use super::{MergeEntry, MergeRefusal, Unmerged};

/// The only key path of this format.
pub(super) const KEY: &str = "Puddle";

/// The line above Puddle's block.
pub(super) const BEGIN: &str = "# >>> Puddle shell >>>";

/// The line below Puddle's block.
pub(super) const END: &str = "# <<< Puddle shell <<<";

const SHELL: Markers = Markers {
    begin: BEGIN,
    end: END,
};

/// Why `value` cannot be a block, or `None` when it can.
pub(super) fn invalid(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("the block is empty");
    }
    if value.len() > knownhosts::MAX_BLOCK {
        return Some("the block is too large");
    }
    if value.contains('\0') {
        return Some("the block holds a NUL byte");
    }
    value
        .split('\n')
        .any(|line| matches!(line.trim_end_matches('\r'), BEGIN | END))
        .then_some("the block holds one of its own marker lines")
}

/// The file with the block set.
pub(super) fn apply(
    existing: Option<&[u8]>,
    entries: &[MergeEntry],
) -> Result<String, MergeRefusal> {
    knownhosts::apply_with(&SHELL, existing, entries)
}

/// Removes Puddle's block; whether the file is empty afterwards.
pub(super) fn unmerge(existing: &[u8]) -> Result<Unmerged, MergeRefusal> {
    knownhosts::unmerge_with(&SHELL, existing)
}

#[cfg(test)]
mod tests;
