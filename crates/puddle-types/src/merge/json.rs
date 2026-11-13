// SPDX-License-Identifier: AGPL-3.0-or-later
//! The JSON engine: edits the text in place, so every byte outside the owned keys stays.
//!
//! Each edit validates the whole text with `serde_json` first, then scans it (iteratively, so
//! deep nesting can't overflow the stack) to find the byte spans of the members on an owned
//! path, and splices only those spans. After all edits, the result is parsed again and compared
//! with the same edits done on the parsed value; any difference refuses the merge
//! ([`MergeRefusal::Inconsistent`]) instead of writing a file Puddle can't vouch for.
//!
//! The same engine serves JSONC ([`Dialect::Comments`]): it reads a same-length *view* of the
//! text with the comments and trailing commas blanked (see `jsonc`), and splices the original,
//! so comments stay where the user put them.

use serde_json::{Map, Value};

use super::jsonc::{Blanked, blank};
use super::{MergeEntry, MergeRefusal, display_key};

/// What the file may contain besides strict JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Dialect {
    /// RFC 8259 only.
    Strict,
    /// Also `//` and `/* */` comments and trailing commas (VS Code settings).
    Comments,
}

impl Dialect {
    fn name(self) -> &'static str {
        match self {
            Self::Strict => "JSON",
            Self::Comments => "JSONC",
        }
    }

    fn blank(self, text: &str) -> Result<Blanked, MergeRefusal> {
        match self {
            Self::Strict => Ok(Blanked::strict(text)),
            Self::Comments => blank(text),
        }
    }
}

/// Indent unit for new members when the file shows none (the Docker CLI writes tabs).
const DEFAULT_UNIT: &str = "\t";

/// Applies a spec: removes `dropped`, sets every entry. `None` in means no file; the result is
/// `None` only when nothing was written and nothing exists.
pub(super) fn apply(
    d: Dialect,
    original: Option<&str>,
    dropped: &[&[String]],
    entries: &[MergeEntry],
) -> Result<Option<String>, MergeRefusal> {
    let mut text = original.unwrap_or("{}\n").to_owned();
    if only_comments(d, &text)? {
        // A settings file that holds nothing but comments is an empty one: the object goes
        // below them.
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str("{}\n");
    }
    let mut expected = parse_root(d, &text)?;
    for key in dropped {
        let (t, top) = remove(d, &text, key)?;
        text = t;
        if let Some(len) = top {
            value_delete(&mut expected, key.get(..len).unwrap_or(key));
        }
    }
    for e in entries {
        let value: Value =
            serde_json::from_str(&e.value).map_err(|_| MergeRefusal::Inconsistent)?;
        text = set(d, &text, &e.key, &value)?;
        value_set(&mut expected, &e.key, value);
    }
    check(d, &text, &expected)?;
    Ok(Some(text))
}

/// Whether a JSONC file has comments and nothing else.
fn only_comments(d: Dialect, text: &str) -> Result<bool, MergeRefusal> {
    let b = d.blank(text)?;
    Ok(!b.comments.is_empty() && b.view.trim().is_empty())
}

/// Removes `keys` (and objects left empty on their paths). Returns the text and whether the top
/// level is empty.
pub(super) fn unmerge(
    d: Dialect,
    original: &str,
    keys: &[&[String]],
) -> Result<(String, bool), MergeRefusal> {
    if only_comments(d, original)? {
        return Ok((original.to_owned(), false));
    }
    let mut expected = parse_root(d, original)?;
    let mut text = original.to_owned();
    for key in keys {
        let (t, top) = remove(d, &text, key)?;
        text = t;
        if let Some(len) = top {
            value_delete(&mut expected, key.get(..len).unwrap_or(key));
        }
    }
    check(d, &text, &expected)?;
    // A file that still has the user's comments is not empty.
    let empty =
        expected.as_object().is_some_and(Map::is_empty) && d.blank(&text)?.comments.is_empty();
    Ok((text, empty))
}

fn parse_root(d: Dialect, text: &str) -> Result<Value, MergeRefusal> {
    let view = d.blank(text)?.view;
    let v: Value = serde_json::from_str(&view).map_err(|e| MergeRefusal::Syntax {
        format: d.name(),
        reason: e.to_string(),
    })?;
    if v.is_object() {
        Ok(v)
    } else {
        Err(MergeRefusal::NotAnObject)
    }
}

fn check(d: Dialect, text: &str, expected: &Value) -> Result<(), MergeRefusal> {
    let parsed = serde_json::from_str::<Value>(&d.blank(text)?.view);
    ensure(parsed.is_ok_and(|v| &v == expected))
}

// --- The value-level twin of each edit, for the self-check ------------------------------------

fn value_set(root: &mut Value, key: &[String], value: Value) {
    if let Some((last, parents)) = key.split_last() {
        let mut cur = Some(root);
        for k in parents {
            cur = cur.and_then(Value::as_object_mut).map(|obj| {
                obj.entry(k.clone())
                    .or_insert_with(|| Value::Object(Map::new()))
            });
        }
        if let Some(obj) = cur.and_then(Value::as_object_mut) {
            obj.insert(last.clone(), value);
        }
    }
}

/// Removes `key` (and nothing else) from the value tree.
fn value_delete(root: &mut Value, key: &[String]) {
    if let Some((last, parents)) = key.split_last() {
        let mut cur = Some(root);
        for k in parents {
            cur = cur
                .and_then(Value::as_object_mut)
                .and_then(|obj| obj.get_mut(k));
        }
        if let Some(obj) = cur.and_then(Value::as_object_mut) {
            obj.remove(last);
        }
    }
}

// --- The scanner -----------------------------------------------------------------------------

/// One `"key": value` member: byte offsets into the text.
struct Member {
    key: String,
    key_start: usize,
    value_start: usize,
    value_end: usize,
}

/// An object: the offsets of its braces and its members.
struct Object {
    open: usize,
    close: usize,
    members: Vec<Member>,
}

/// The text is valid JSON when the scanner runs, so a surprise is a bug, not bad input.
fn ensure(holds: bool) -> Result<(), MergeRefusal> {
    holds.then_some(()).ok_or(MergeRefusal::Inconsistent)
}

fn at(b: &[u8], i: usize) -> Option<u8> {
    b.get(i).copied()
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while matches!(at(b, i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        i += 1;
    }
    i
}

/// `i` is on a `"`; returns the offset after the closing one.
fn skip_string(b: &[u8], mut i: usize) -> Result<usize, MergeRefusal> {
    i += 1;
    loop {
        match at(b, i).ok_or(MergeRefusal::Inconsistent)? {
            b'\\' => i += 2,
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
}

/// `i` is on the first byte of a value; returns the offset after it.
fn skip_value(b: &[u8], mut i: usize) -> Result<usize, MergeRefusal> {
    match at(b, i).ok_or(MergeRefusal::Inconsistent)? {
        b'"' => skip_string(b, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            loop {
                match at(b, i).ok_or(MergeRefusal::Inconsistent)? {
                    b'"' => {
                        i = skip_string(b, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Ok(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        _ => {
            while let Some(c) = at(b, i) {
                if matches!(c, b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r') {
                    break;
                }
                i += 1;
            }
            Ok(i)
        }
    }
}

/// The object whose `{` is at `open`.
fn object_at(text: &str, open: usize) -> Result<Object, MergeRefusal> {
    let b = text.as_bytes();
    ensure(at(b, open) == Some(b'{'))?;
    let mut members = Vec::new();
    let mut i = open + 1;
    loop {
        i = skip_ws(b, i);
        if at(b, i) == Some(b'}') {
            return Ok(Object {
                open,
                close: i,
                members,
            });
        }
        ensure(at(b, i) == Some(b'"'))?;
        let key_start = i;
        let key_end = skip_string(b, i)?;
        let raw = text
            .get(key_start..key_end)
            .ok_or(MergeRefusal::Inconsistent)?;
        let key: String = serde_json::from_str(raw).map_err(|_| MergeRefusal::Inconsistent)?;
        i = skip_ws(b, key_end);
        ensure(at(b, i) == Some(b':'))?;
        let value_start = skip_ws(b, i + 1);
        let value_end = skip_value(b, value_start)?;
        members.push(Member {
            key,
            key_start,
            value_start,
            value_end,
        });
        i = skip_ws(b, value_end);
        if at(b, i) == Some(b',') {
            i += 1;
        }
    }
}

fn root(text: &str) -> Result<Object, MergeRefusal> {
    object_at(text, skip_ws(text.as_bytes(), 0))
}

/// Where an owned path stands in the file.
enum Found {
    /// The member for the whole path: `obj.members[idx]`.
    Member { obj: Object, idx: usize },
    /// `obj` is the object at `key[..depth]`, and it has no member `key[depth]`.
    Missing { obj: Object, depth: usize },
}

fn find(text: &str, key: &[String]) -> Result<Found, MergeRefusal> {
    let mut obj = root(text)?;
    for (depth, k) in key.iter().enumerate() {
        let mut hits = obj
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| &m.key == k)
            .map(|(i, _)| i);
        let Some(idx) = hits.next() else {
            return Ok(Found::Missing { obj, depth });
        };
        let shown = || display_key(key.get(..=depth).unwrap_or(key));
        if hits.next().is_some() {
            return Err(MergeRefusal::DuplicateKey { key: shown() });
        }
        if depth + 1 == key.len() {
            return Ok(Found::Member { obj, idx });
        }
        let m = obj.members.get(idx).ok_or(MergeRefusal::Inconsistent)?;
        if at(text.as_bytes(), m.value_start) != Some(b'{') {
            return Err(MergeRefusal::Conflict { key: shown() });
        }
        obj = object_at(text, m.value_start)?;
    }
    // Only an empty path gets here: every other one returns from inside the loop.
    Err(MergeRefusal::Inconsistent)
}

// --- Layout ----------------------------------------------------------------------------------

/// The whitespace that starts the line holding offset `pos`.
fn line_indent(text: &str, pos: usize) -> &str {
    let head = text.get(..pos).unwrap_or("");
    let line = head
        .rfind('\n')
        .map_or(head, |n| head.get(n + 1..).unwrap_or(""));
    let end = line
        .find(|c: char| c != ' ' && c != '\t')
        .unwrap_or(line.len());
    line.get(..end).unwrap_or("")
}

/// Whether `pos` is the first non-blank byte on its line, after `after` (i.e. on a later line).
fn starts_line(text: &str, pos: usize, after: usize) -> bool {
    let head = text.get(..pos).unwrap_or("");
    match head.rfind('\n') {
        Some(n) if n >= after => head
            .get(n + 1..)
            .is_some_and(|s| s.bytes().all(|c| c == b' ' || c == b'\t')),
        _ => false,
    }
}

/// How much deeper `obj`'s members are indented than the object itself, when its first member
/// starts a line.
fn own_unit(text: &str, obj: &Object) -> Option<String> {
    let first = obj.members.first()?;
    if !starts_line(text, first.key_start, obj.open) {
        return None;
    }
    line_indent(text, first.key_start)
        .strip_prefix(line_indent(text, obj.open))
        .filter(|u| !u.is_empty())
        .map(str::to_owned)
}

/// The indent unit for new lines in `obj`: its own, else the top level's, else a tab.
fn indent_unit(text: &str, obj: &Object) -> String {
    own_unit(text, obj)
        .or_else(|| root(text).ok().and_then(|r| own_unit(text, &r)))
        .unwrap_or_else(|| DEFAULT_UNIT.to_owned())
}

/// `value` on lines indented by `unit`, continuation lines starting with `base`.
fn pretty(value: &Value, unit: &str, base: &str) -> String {
    let mut out = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(unit.as_bytes());
    let mut ser = serde_json::Serializer::with_formatter(&mut out, fmt);
    // Serialising a `Value` into a Vec can't fail.
    let text = serde::Serialize::serialize(value, &mut ser)
        .map(|()| String::from_utf8(out).unwrap_or_default())
        .unwrap_or_default();
    text.replace('\n', &format!("\n{base}"))
}

fn quoted(key: &str) -> String {
    Value::String(key.to_owned()).to_string()
}

fn splice(text: &str, start: usize, end: usize, with: &str) -> Result<String, MergeRefusal> {
    let head = text.get(..start).ok_or(MergeRefusal::Inconsistent)?;
    let tail = text.get(end..).ok_or(MergeRefusal::Inconsistent)?;
    Ok([head, with, tail].concat())
}

// --- Edits -----------------------------------------------------------------------------------

/// Sets `key` to `value`, creating the objects on its path. An equal value is left as it is.
fn set(d: Dialect, text: &str, key: &[String], value: &Value) -> Result<String, MergeRefusal> {
    let blanked = d.blank(text)?;
    let view = blanked.view.as_str();
    match find(view, key)? {
        Found::Member { obj, idx } => {
            let unit = indent_unit(view, &obj);
            let m = obj.members.get(idx).ok_or(MergeRefusal::Inconsistent)?;
            let old = view
                .get(m.value_start..m.value_end)
                .ok_or(MergeRefusal::Inconsistent)?;
            if serde_json::from_str::<Value>(old).ok().as_ref() == Some(value) {
                return Ok(text.to_owned());
            }
            let rendered = if starts_line(view, m.key_start, obj.open) {
                pretty(value, &unit, line_indent(view, m.key_start))
            } else {
                value.to_string()
            };
            splice(text, m.value_start, m.value_end, &rendered)
        }
        Found::Missing { obj, depth } => {
            let (name, below) = key
                .get(depth..)
                .and_then(<[String]>::split_first)
                .ok_or(MergeRefusal::Inconsistent)?;
            let mut nested = value.clone();
            for k in below.iter().rev() {
                let mut map = Map::new();
                map.insert(k.clone(), nested);
                nested = Value::Object(map);
            }
            insert(
                text,
                &blanked,
                &obj,
                name,
                &nested,
                &indent_unit(view, &obj),
            )
        }
    }
}

/// Adds `"name": value` as the last member of `obj`, in the object's layout.
fn insert(
    text: &str,
    blanked: &Blanked,
    obj: &Object,
    name: &str,
    value: &Value,
    unit: &str,
) -> Result<String, MergeRefusal> {
    let view = blanked.view.as_str();
    let Some(last) = obj.members.last() else {
        let outer = line_indent(view, obj.open);
        let inner = format!("{outer}{unit}");
        let member = format!("{}: {}", quoted(name), pretty(value, unit, &inner));
        let between = text.get(obj.open + 1..obj.close).unwrap_or("");
        return if between.trim().is_empty() {
            // An empty object becomes a multi-line one.
            splice(
                text,
                obj.open + 1,
                obj.close,
                &format!("\n{inner}{member}\n{outer}"),
            )
        } else {
            // Only comments inside: the member goes in front of them.
            splice(
                text,
                obj.open + 1,
                obj.open + 1,
                &format!("\n{inner}{member}"),
            )
        };
    };
    if !starts_line(view, last.key_start, obj.open) {
        let member = format!(", {}: {value}", quoted(name));
        return splice(text, last.value_end, last.value_end, &member);
    }
    let indent = line_indent(view, last.key_start);
    let member = format!(
        "\n{indent}{}: {}",
        quoted(name),
        pretty(value, unit, indent)
    );
    match end_of_line(text, blanked, last.value_end) {
        // The member goes on the line after the last one, so a comment trailing the last
        // member stays with it.
        Some((eol, has_comma))
            if !blanked
                .comments
                .iter()
                .any(|c| c.start < eol && eol < c.end) =>
        {
            let text = splice(text, eol, eol, &member)?;
            if has_comma {
                Ok(text)
            } else {
                splice(&text, last.value_end, last.value_end, ",")
            }
        }
        _ => {
            let member = format!(",{member}");
            splice(text, last.value_end, last.value_end, &member)
        }
    }
}

/// From `from` (the end of a value) to the end of its line, when only spaces, a comma and
/// comments follow: the offset of the line break (or of the end), and whether a comma was there
/// (the view has blanked a trailing one, so look in the text).
fn end_of_line(text: &str, blanked: &Blanked, from: usize) -> Option<(usize, bool)> {
    let rest = blanked.view.get(from..)?;
    let end = from + rest.find(['\n', '\r']).unwrap_or(rest.len());
    let only_blank = text
        .get(from..end)?
        .bytes()
        .enumerate()
        .all(|(i, b)| matches!(b, b' ' | b'\t' | b',') || in_comment(blanked, from + i));
    let comma =
        (from..end).any(|i| text.as_bytes().get(i) == Some(&b',') && !in_comment(blanked, i));
    only_blank.then_some((end, comma))
}

fn in_comment(blanked: &Blanked, at: usize) -> bool {
    blanked.comments.iter().any(|c| c.contains(&at))
}

/// Removes `key`, then each object on its path that is left empty and has no comment in it. A
/// missing key, or a path through a non-object, leaves the text as it is. The second value is
/// the length of the shortest path that went (`None`: nothing did), for the value-level twin.
fn remove(d: Dialect, text: &str, key: &[String]) -> Result<(String, Option<usize>), MergeRefusal> {
    let mut text = text.to_owned();
    let mut top = None;
    for len in (1..=key.len()).rev() {
        let path = key.get(..len).ok_or(MergeRefusal::Inconsistent)?;
        let blanked = d.blank(&text)?;
        let view = blanked.view.as_str();
        let (obj, idx) = match find(view, path) {
            Ok(Found::Member { obj, idx }) => (obj, idx),
            Ok(Found::Missing { .. }) | Err(MergeRefusal::Conflict { .. }) => {
                return Ok((text, top));
            }
            Err(e) => return Err(e),
        };
        if len < key.len() {
            // A parent: remove it only when the removal below left it empty, comments and all.
            let m = obj.members.get(idx).ok_or(MergeRefusal::Inconsistent)?;
            let inner = object_at(view, m.value_start)?;
            if !inner.members.is_empty() || blanked.has_comment_in(inner.open..inner.close) {
                return Ok((text, top));
            }
        }
        text = remove_member(&text, &blanked, &obj, idx)?;
        top = Some(len);
    }
    Ok((text, top))
}

fn remove_member(
    text: &str,
    blanked: &Blanked,
    obj: &Object,
    idx: usize,
) -> Result<String, MergeRefusal> {
    let m = &obj.members;
    let this = m.get(idx).ok_or(MergeRefusal::Inconsistent)?;
    let (start, end, joined) = match (idx.checked_sub(1).and_then(|p| m.get(p)), m.get(idx + 1)) {
        // The only member: the object becomes `{}`.
        (None, None) => (obj.open + 1, obj.close, false),
        // The first of several: up to the next key, so the next one takes its place.
        (None, Some(next)) => (this.key_start, next.key_start, false),
        // Any later one: from the end of the previous value (its comma goes with it).
        (Some(prev), _) => (prev.value_end, this.value_end, true),
    };
    // The user's comments between the members stay; the removed member's own do not.
    let kept = kept_comments(
        text,
        blanked,
        start..end,
        this.key_start..this.value_end,
        joined,
    );
    splice(text, start, end, &kept)
}

/// The comments in `span` that are not inside `owned`, as text to put back where the span was.
fn kept_comments(
    text: &str,
    blanked: &Blanked,
    span: std::ops::Range<usize>,
    owned: std::ops::Range<usize>,
    joined: bool,
) -> String {
    let mut out = String::new();
    let comments = blanked
        .comments
        .iter()
        .filter_map(|c| Some((c, text.get(c.clone())?)));
    for (c, t) in comments {
        let inside_span = c.start >= span.start && c.end <= span.end;
        let inside_owned = c.start >= owned.start && c.end <= owned.end;
        if !inside_span || inside_owned {
            continue;
        }
        if out.is_empty() && joined {
            out.push(' ');
        }
        out.push_str(t);
        out.push(if t.starts_with("//") { '\n' } else { ' ' });
    }
    if out.ends_with('\n') {
        // The line comment needs its break, unless one follows anyway; then the next member
        // keeps the indent it had.
        let after = text.get(span.end..).unwrap_or("");
        if after.is_empty() || after.starts_with(['\n', '\r']) {
            out.pop();
        } else if let Some(indent) = text
            .get(span.clone())
            .and_then(|s| s.rsplit_once('\n'))
            .map(|(_, tail)| tail)
            .filter(|tail| tail.chars().all(|c| c == ' ' || c == '\t'))
        {
            out.push_str(indent);
        }
    }
    out
}

#[cfg(test)]
mod tests;
