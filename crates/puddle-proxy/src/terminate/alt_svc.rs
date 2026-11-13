// SPDX-License-Identifier: AGPL-3.0-or-later
//! `Alt-Svc` on responses Puddle decrypts: the HTTP/3 alternatives are removed.
//!
//! A workspace has no UDP path out, so a client that is told "this server speaks HTTP/3" would
//! try it, fail and only then use TCP. Removing the `h3` entries (and the draft and Google
//! variants of them) spares it the attempt; every other entry, and every other field, goes to the
//! guest as the server sent it. A spliced connection is not touched: Puddle does not see inside it,
//! and the client falls back on its own when its QUIC attempt fails.

use ::http::HeaderMap;
use ::http::header::{ALT_SVC, HeaderValue};

/// Removes the HTTP/3 entries from every `Alt-Svc` field of `headers` and returns how many entries
/// went. A field whose entries all go is removed; a field with nothing to remove is left as it is.
pub(super) fn strip_h3(headers: &mut HeaderMap) -> usize {
    let mut kept = Vec::new();
    let mut removed = 0;
    for value in headers.get_all(ALT_SVC) {
        match without_h3(value.as_bytes()) {
            Some(stripped) => {
                removed += stripped.removed;
                kept.extend(stripped.rest);
            }
            None => kept.push(value.clone()),
        }
    }
    if removed > 0 {
        headers.remove(ALT_SVC);
        for value in kept {
            headers.append(ALT_SVC, value);
        }
        tracing::debug!(removed, "removed HTTP/3 from Alt-Svc");
    }
    removed
}

/// What is left of one field after its HTTP/3 entries are removed.
struct Stripped {
    /// `None` when no entry is left.
    rest: Option<HeaderValue>,
    removed: usize,
}

/// `value` without its HTTP/3 entries, or `None` when it has none.
fn without_h3(value: &[u8]) -> Option<Stripped> {
    let mut kept: Vec<&[u8]> = Vec::new();
    let mut removed = 0;
    for entry in entries(value) {
        if is_h3(entry) {
            removed += 1;
        } else {
            kept.push(entry);
        }
    }
    (removed > 0).then(|| Stripped {
        rest: (!kept.is_empty())
            .then(|| HeaderValue::from_bytes(&kept.join(&b", "[..])).ok())
            .flatten(),
        removed,
    })
}

/// The comma-separated entries of a field value, each without its surrounding blanks. A comma
/// inside a quoted string (`v="46,43"`) does not end an entry, and neither does a quote that is
/// never closed: the rest of the value is then one entry.
fn entries(value: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut rest = Some(value);
    std::iter::from_fn(move || {
        let current = rest.take()?;
        let mut quoted = false;
        let mut escaped = false;
        for (at, &byte) in current.iter().enumerate() {
            match (quoted, escaped, byte) {
                (true, true, _) => escaped = false,
                (true, false, b'\\') => escaped = true,
                (true, false, b'"') => quoted = false,
                (false, _, b'"') => quoted = true,
                (false, _, b',') => {
                    let (head, tail) = current.split_at(at);
                    rest = tail.split_first().map(|(_, after)| after);
                    return Some(head.trim_ascii());
                }
                _ => {}
            }
        }
        Some(current.trim_ascii())
    })
}

/// Whether an entry offers HTTP/3: its protocol id (the part before `=`, percent-encoded by the
/// server's choice) is `h3`, a draft such as `h3-29` or `h3-Q050`, or Google's older `quic`.
fn is_h3(entry: &[u8]) -> bool {
    let mut parts = entry.splitn(2, |&byte| byte == b'=');
    let (Some(id), Some(_)) = (parts.next(), parts.next()) else {
        return false;
    };
    let id = percent_decoded(id);
    id == b"h3" || id == b"quic" || id.starts_with(b"h3-")
}

fn percent_decoded(raw: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(raw.len());
    let mut bytes = raw.iter().copied();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let mut ahead = bytes.clone();
            if let (Some(high), Some(low)) =
                (ahead.next().and_then(hex), ahead.next().and_then(hex))
            {
                out.push((high << 4) | low);
                bytes = ahead;
                continue;
            }
        }
        out.push(byte);
    }
    out
}

fn hex(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

#[cfg(test)]
mod tests;
