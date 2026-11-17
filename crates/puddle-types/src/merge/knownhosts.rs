// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ssh_known_hosts`: Puddle owns the lines between two marker lines and nothing else.
//!
//! The file is the image's and the user's, so every byte outside the markers stays as it is. A
//! file without markers gets the block at its end; a block that is already there (one begin line,
//! then one end line) is replaced where it stands; a file whose markers were damaged (one
//! removed, a block duplicated, the end before the begin) loses each begin-to-end block and each
//! lone marker line, keeps every other line (also one between two blocks), and gets the block at
//! its end again. The file is line-based, so no
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

/// Which marker a line is, if any.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Begin,
    End,
}

fn marker(m: &Markers, line: &str) -> Option<Marker> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line == m.begin {
        Some(Marker::Begin)
    } else if line == m.end {
        Some(Marker::End)
    } else {
        None
    }
}

/// The file without Puddle's lines, and the byte offset where they were. Puddle's lines are each
/// begin line whose next marker is an end line, through that end line, and every marker line
/// that is not part of such a pair. Every other line is the user's and stays, also one between
/// two blocks. `in_place` says the file held exactly one block and no lone marker.
fn without(m: &Markers, lines: &[&str]) -> (String, usize, bool) {
    let kinds: Vec<Option<Marker>> = lines.iter().map(|line| marker(m, line)).collect();
    let mut rest = String::new();
    let mut at = None;
    let mut blocks = 0;
    let mut lone = 0;
    let mut i = 0;
    while let Some(line) = lines.get(i) {
        match kinds.get(i).copied().flatten() {
            None => {
                rest.push_str(line);
                i += 1;
            }
            Some(Marker::Begin) => {
                at.get_or_insert(rest.len());
                let next = kinds.iter().skip(i + 1).position(Option::is_some);
                match next.map(|n| i + 1 + n) {
                    Some(end) if kinds.get(end).copied().flatten() == Some(Marker::End) => {
                        blocks += 1;
                        i = end + 1;
                    }
                    _ => {
                        lone += 1;
                        i += 1;
                    }
                }
            }
            Some(Marker::End) => {
                at.get_or_insert(rest.len());
                lone += 1;
                i += 1;
            }
        }
    }
    let in_place = blocks == 1 && lone == 0;
    let at = at.unwrap_or(rest.len());
    (rest, at, in_place)
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
