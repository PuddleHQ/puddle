// SPDX-License-Identifier: GPL-3.0-or-later
//! `ssh_known_hosts`: Puddle owns the lines between two marker lines and nothing else.
//!
//! The file is the image's and the user's, so every byte outside the markers stays as it is. A
//! file without markers gets the block at its end; a block that is already there (one begin line,
//! then one end line) is replaced where it stands; a file whose markers were damaged (one
//! removed, one duplicated, the end before the begin) loses every line from the first marker line
//! to the last one, and the block is written at the end again. The file is line-based, so no
//! content makes it unparsable; only a NUL byte, invalid UTF-8 or more than [`crate::MAX_MERGE_FILE`]
//! bytes is refused.

use super::{MergeEntry, MergeRefusal, Merged, Unmerged, read_text};

/// The only key path of this format.
pub(super) const KEY: &str = "Puddle";

/// The line above Puddle's block.
pub(super) const BEGIN: &str = "# puddle: begin";

/// The line below Puddle's block.
pub(super) const END: &str = "# puddle: end";

/// The two lines that fence a block. The engine is the same for every line-based file whose
/// block is fenced by comment lines; `shellblock` reuses it with `#`-comment markers of its own.
pub(super) struct Markers {
    pub(super) begin: &'static str,
    pub(super) end: &'static str,
}

/// The markers of `ssh_known_hosts`.
const HOSTS: Markers = Markers {
    begin: BEGIN,
    end: END,
};

/// The most a block may hold, so a rewrite never nears the file limit by itself.
pub(super) const MAX_BLOCK: usize = 256 << 10;

/// Why `value` cannot be a block, or `None` when it can: lines of host-key entries, each
/// non-empty and none a comment, with no control character but the line break between them.
pub(super) fn invalid(value: &str) -> Option<&'static str> {
    if value.is_empty() {
        return Some("the block is empty");
    }
    if value.len() > MAX_BLOCK {
        return Some("the block is too large");
    }
    if value.chars().any(|c| c.is_control() && c != '\n') {
        return Some("the block holds a control character");
    }
    let bad = |line: &str| line.trim().is_empty() || line.trim_start().starts_with('#');
    value
        .split('\n')
        .any(bad)
        .then_some("every line must be a host-key entry")
}

/// The block with its markers.
fn block(m: &Markers, entries: &[MergeEntry]) -> String {
    let body = entries.first().map_or("", MergeEntry::value);
    format!("{}\n{body}\n{}\n", m.begin, m.end)
}

/// The indexes of the lines that are one of Puddle's markers.
fn markers(m: &Markers, lines: &[&str]) -> Vec<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| is_marker(m, line))
        .map(|(i, _)| i)
        .collect()
}

fn is_marker(m: &Markers, line: &str) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    line == m.begin || line == m.end
}

/// The file without Puddle's lines, and the byte offset where they were. Puddle's lines are one
/// begin and one end line in order, with whatever stands between them; with damaged markers they
/// are every line from the first marker to the last, and a lone marker is a span of one line.
/// `in_place` says the span is a well-formed block.
fn without(m: &Markers, lines: &[&str]) -> (String, usize, bool) {
    let found = markers(m, lines);
    let (Some(&first), Some(&last)) = (found.first(), found.last()) else {
        return (lines.concat(), lines.iter().map(|l| l.len()).sum(), false);
    };
    let well_formed = found.len() == 2
        && lines
            .get(first)
            .is_some_and(|l| l.trim_end_matches(['\r', '\n']) == m.begin)
        && lines
            .get(last)
            .is_some_and(|l| l.trim_end_matches(['\r', '\n']) == m.end);
    let before: String = lines.iter().take(first).copied().collect();
    let at = before.len();
    let after: String = lines.iter().skip(last + 1).copied().collect();
    let rest = [before, after].concat();
    (rest, at, well_formed)
}

/// The file with the block set.
pub(super) fn apply(
    existing: Option<&[u8]>,
    entries: &[MergeEntry],
) -> Result<String, MergeRefusal> {
    apply_with(&HOSTS, existing, entries)
}

/// The file with the block fenced by `m` set.
pub(super) fn apply_with(
    m: &Markers,
    existing: Option<&[u8]>,
    entries: &[MergeEntry],
) -> Result<String, MergeRefusal> {
    let original = read_text(existing)?.unwrap_or("");
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    let (mut out, at, well_formed) = without(m, &lines);
    let ours = block(m, entries);
    if well_formed {
        out.insert_str(at, &ours);
        return Ok(out);
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&ours);
    Ok(out)
}

/// Removes Puddle's block; whether the file is empty afterwards.
pub(super) fn unmerge(existing: &[u8]) -> Result<Unmerged, MergeRefusal> {
    unmerge_with(&HOSTS, existing)
}

/// Removes the block fenced by `m`; whether the file is empty afterwards.
pub(super) fn unmerge_with(m: &Markers, existing: &[u8]) -> Result<Unmerged, MergeRefusal> {
    let original = read_text(Some(existing))?.unwrap_or("");
    let lines: Vec<&str> = original.split_inclusive('\n').collect();
    let (out, ..) = without(m, &lines);
    let empty = out.trim().is_empty();
    let merged = if out == original {
        Merged::Unchanged
    } else {
        Merged::Write(out.into_bytes())
    };
    Ok(Unmerged { merged, empty })
}

#[cfg(test)]
mod tests;
