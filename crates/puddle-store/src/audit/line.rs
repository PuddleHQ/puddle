// SPDX-License-Identifier: AGPL-3.0-or-later
//! The text of an audit line: string caps, the JSON escape rules and the encoder behind
//! [`super::AuditRecord::to_line`] (R-23, R-26).

use std::io;

use serde::Serialize;
use serde_json::Value;

use super::AuditError;

/// Cuts every string over `max` bytes, recording the JSON pointer of each one cut.
pub(super) fn cap_strings(value: &mut Value, max: usize, pointer: &str, cut: &mut Vec<String>) {
    match value {
        Value::String(s) if s.len() > max => {
            let keep = s.floor_char_boundary(max);
            s.truncate(keep);
            cut.push(pointer.to_owned());
        }
        Value::Object(map) => {
            for (key, child) in map {
                cap_strings(child, max, &format!("{pointer}/{key}"), cut);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                cap_strings(child, max, &format!("{pointer}/{index}"), cut);
            }
        }
        _ => {}
    }
}

/// The JSON pointer and encoded length of the non-empty string whose encoding is longest.
pub(super) fn longest_string(value: &Value, pointer: &str) -> Option<(usize, String)> {
    match value {
        Value::String(s) if !s.is_empty() => Some((encoded_len(s), pointer.to_owned())),
        Value::Object(map) => map
            .iter()
            .filter_map(|(key, child)| longest_string(child, &format!("{pointer}/{key}")))
            .max_by_key(|(len, _)| *len),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .filter_map(|(index, child)| longest_string(child, &format!("{pointer}/{index}")))
            .max_by_key(|(len, _)| *len),
        _ => None,
    }
}

pub(super) fn encoded_len(s: &str) -> usize {
    s.chars()
        .map(|c| if needs_escape(c) { 6 } else { c.len_utf8() })
        .sum()
}

pub(super) fn needs_escape(c: char) -> bool {
    c.is_control() || c == '"' || c == '\\' || c == '\u{2028}' || c == '\u{2029}'
}

pub(super) fn encode(value: &Value) -> Result<String, AuditError> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, EscapingFormatter);
    value.serialize(&mut ser)?;
    // serde_json only ever writes UTF-8.
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// `serde_json`'s compact formatter, plus `\uXXXX` for DEL, C1 controls and U+2028/U+2029,
/// which `serde_json` leaves raw. C0 controls are already escaped before they get here.
pub(super) struct EscapingFormatter;

impl serde_json::ser::Formatter for EscapingFormatter {
    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut start = 0;
        for (index, c) in fragment.char_indices() {
            if needs_escape(c) {
                writer.write_all(fragment.get(start..index).unwrap_or_default().as_bytes())?;
                write!(writer, "\\u{:04x}", u32::from(c))?;
                start = index + c.len_utf8();
            }
        }
        writer.write_all(fragment.get(start..).unwrap_or_default().as_bytes())
    }
}
