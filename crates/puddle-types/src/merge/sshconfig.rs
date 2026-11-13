// SPDX-License-Identifier: AGPL-3.0-or-later
//! `ssh_config`: Puddle owns one `Include <pattern>` line, so `ssh` reads the drop-in folder
//! Puddle writes into.
//!
//! The file is the image's, so nothing else in it is touched: when no `Include` that reaches the
//! pattern stands before the first `Host` or `Match` (an `Include` inside a block only applies
//! to that block), the line goes at the very top, behind one comment that says who added it and
//! why, and every other byte of the file stays where it is. The top is where the distributions
//! put it, and the first value `ssh` reads for an option wins, so Puddle's drop-in then comes
//! before the system defaults.
//!
//! A file with a NUL byte, invalid UTF-8 or more than [`crate::MAX_MERGE_FILE`] bytes is refused.

use super::{MergeEntry, MergeRefusal, Merged, Unmerged, read_text};

/// The comment above the line Puddle adds. Removal finds the pair by it, so it never changes.
pub(super) const MARKER: &str = "# Added by Puddle at boot: this image's ssh_config did not read the folder that holds Puddle's ssh settings.";

/// The only key path of this format.
pub(super) const KEY: &str = "Include";

/// The pattern as the entry holds it.
fn pattern(entries: &[MergeEntry]) -> &str {
    entries.first().map_or("", MergeEntry::value)
}

/// The file with the line set: as it is when an `Include` already reaches the pattern, else the
/// line behind its comment on top.
pub(super) fn apply(
    existing: Option<&[u8]>,
    entries: &[MergeEntry],
) -> Result<String, MergeRefusal> {
    let original = read_text(existing)?.unwrap_or("");
    let wanted = pattern(entries);
    if reaches(original, wanted) {
        return Ok(original.to_owned());
    }
    Ok(format!("{MARKER}\nInclude {wanted}\n{original}"))
}

/// Removes the pair Puddle added; whether the file is empty afterwards. A pair the user edited
/// or moved is not Puddle's any more and stays.
pub(super) fn unmerge(existing: &[u8]) -> Result<Unmerged, MergeRefusal> {
    let original = read_text(Some(existing))?.unwrap_or("");
    let (out, empty) = remove_pair(original);
    let merged = if out == original {
        Merged::Unchanged
    } else {
        Merged::Write(out.into_bytes())
    };
    Ok(Unmerged { merged, empty })
}

/// `text` without the first marker comment and the `Include` line right behind it.
fn remove_pair(text: &str) -> (String, bool) {
    let mut out = String::with_capacity(text.len());
    let mut lines = text.split_inclusive('\n').peekable();
    while let Some(line) = lines.next() {
        if line.trim_end_matches(['\r', '\n']) == MARKER
            && lines.peek().is_some_and(|next| {
                keyword(next).is_some_and(|k| k.eq_ignore_ascii_case("include"))
            })
        {
            lines.next();
            out.extend(lines);
            break;
        }
        out.push_str(line);
    }
    let empty = out.trim().is_empty();
    (out, empty)
}

/// The first word of a configuration line (`Key value` or `Key=value`); `None` for a blank line
/// or a comment.
fn keyword(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    Some(&line[..end])
}

/// The arguments after the keyword, quotes removed.
fn arguments(line: &str) -> Vec<String> {
    let line = line.trim_start();
    let rest = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .map_or("", |i| &line[i..]);
    rest.trim_start_matches(|c: char| c.is_whitespace() || c == '=')
        .split_whitespace()
        .take_while(|w| !w.starts_with('#'))
        .map(|w| w.trim_matches('"').to_owned())
        .collect()
}

/// Whether an `Include` before the first `Host` or `Match` reads `wanted`'s files: the same
/// pattern, or its folder's `*`. A relative pattern is relative to `/etc/ssh`.
fn reaches(text: &str, wanted: &str) -> bool {
    let folder = wanted.rsplit_once('/').map_or("", |(dir, _)| dir);
    for line in text.lines() {
        let Some(word) = keyword(line) else { continue };
        if word.eq_ignore_ascii_case("host") || word.eq_ignore_ascii_case("match") {
            return false;
        }
        if !word.eq_ignore_ascii_case("include") {
            continue;
        }
        let hit = arguments(line).iter().any(|arg| {
            let arg = if arg.starts_with('/') || arg.starts_with('~') {
                arg.clone()
            } else {
                format!("/etc/ssh/{arg}")
            };
            arg == wanted || (!folder.is_empty() && arg == format!("{folder}/*"))
        });
        if hit {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests;
