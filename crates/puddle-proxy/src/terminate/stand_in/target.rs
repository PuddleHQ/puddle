// SPDX-License-Identifier: AGPL-3.0-or-later
//! The request target: where the opt-in [`Placement::Query`] and [`Placement::Path`] stand-ins
//! are swapped, and how the target is written down for the audit.
//!
//! The target is origin form (`/path?query`) by the time it gets here: both the HTTP/1.1 parser
//! and the HTTP/2 check refuse anything else, and neither lets a fragment through.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError};

use puddle_types::Host;
use zeroize::Zeroizing;

use super::{Placement, StandIn, StandIns, Swapped, percent_decoded, scan, swap_bytes};

impl StandIns {
    /// The request `target` as it goes to `host`: each stand-in that is placed in the path, or in
    /// the one query parameter it names, and is for `host`, swapped for its real value
    /// (percent-encoded where that place needs it). `None` in the first place: nothing was
    /// swapped and the target goes as it came. Every stand-in still in the target after that (a
    /// header one, one toward another host or in another place, a query parameter given twice, a
    /// percent-escaped path) goes out unchanged and is reported.
    pub(crate) fn swap_target(
        &self,
        target: &str,
        host: &Host,
    ) -> (Option<Zeroizing<String>>, Swapped) {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let entries: &[Arc<StandIn>] = &guard;
        let mut report = Swapped::default();
        let (path, query) = match target.split_once('?') {
            Some((path, query)) => (path, Some(query)),
            None => (target, None),
        };
        let new_path = swap_bytes(path.as_bytes(), entries, &mut report, |entry| {
            (entry.placement == Placement::Path && entry.hosts.contains(host))
                .then(|| encoded(entry.real.expose(), keeps_in_path))
        });
        let new_query = query.and_then(|query| swap_query(query, entries, host, &mut report));
        if new_path.is_none() && new_query.is_none() {
            let found = scan(entries, target);
            return (None, found);
        }
        let mut out = Zeroizing::new(String::with_capacity(target.len()));
        match &new_path {
            Some(bytes) => out.push_str(&String::from_utf8_lossy(bytes)),
            None => out.push_str(path),
        }
        if let Some(query) = query {
            out.push('?');
            match &new_query {
                Some(new) => out.push_str(&String::from_utf8_lossy(new)),
                None => out.push_str(query),
            }
        }
        let left = scan(entries, &out);
        for id in left.unswapped {
            Swapped::note(&mut report.unswapped, id);
        }
        (Some(out), report)
    }

    /// `target` as the audit and the logs may show it: every stand-in of this registry in it,
    /// as sent or behind percent-escapes, replaced by `[secret:<name>]` or `[login:<name>]`.
    /// Neither a stand-in nor (as this is the guest's target) a real value is left in it. The rest
    /// stays as sent: decoding it would let the guest put a line break or a `?` in the record.
    pub(crate) fn redact_target<'t>(&self, target: &'t str) -> Cow<'t, str> {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let entries: &[Arc<StandIn>] = &guard;
        if entries.is_empty() {
            return Cow::Borrowed(target);
        }
        // The target with its escapes decoded, and where each decoded byte starts in `target`. A
        // stand-in has no `%`, so one sent as is is found here too.
        let mut decoded = Vec::with_capacity(target.len());
        let mut starts = Vec::with_capacity(target.len() + 1);
        let raw = target.as_bytes();
        let mut at = 0;
        while let Some(&byte) = raw.get(at) {
            starts.push(at);
            if let Some(value) = escaped_byte(raw, at) {
                decoded.push(value);
                at += 3;
            } else {
                decoded.push(byte);
                at += 1;
            }
        }
        starts.push(raw.len());
        let mut spans: Vec<(usize, usize, &StandIn)> = Vec::new();
        for entry in entries {
            let mut from = 0;
            while let Some(found) = decoded.get(from..).and_then(|rest| entry.finder.find(rest)) {
                let begin = from + found;
                from = begin + entry.text.len();
                if let (Some(&a), Some(&b)) = (starts.get(begin), starts.get(from)) {
                    spans.push((a, b, entry));
                }
            }
        }
        if spans.is_empty() {
            return Cow::Borrowed(target);
        }
        spans.sort_by_key(|&(begin, end, _)| (begin, Reverse(end)));
        let mut out = String::with_capacity(target.len());
        let mut copied = 0;
        for (begin, end, entry) in spans {
            if begin < copied {
                continue;
            }
            out.push_str(target.get(copied..begin).unwrap_or_default());
            out.push('[');
            out.push_str(entry.origin.label());
            out.push(':');
            out.push_str(&entry.name);
            out.push(']');
            copied = end;
        }
        out.push_str(target.get(copied..).unwrap_or_default());
        Cow::Owned(out)
    }
}

/// The byte a `%XX` escape at `at` stands for, or `None` when there is no escape there.
fn escaped_byte(raw: &[u8], at: usize) -> Option<u8> {
    if raw.get(at) != Some(&b'%') {
        return None;
    }
    let hex = std::str::from_utf8(raw.get(at + 1..at + 3)?).ok()?;
    u8::from_str_radix(hex, 16).ok()
}

/// The query with each parameter that is exactly a [`Placement::Query`] stand-in for `host`,
/// under the one name that stand-in is placed in, swapped. Names and values are compared with
/// their percent-escapes decoded, as a server reads them. A name given twice is swapped in neither
/// place: servers differ in which one they read. `None` when nothing was swapped.
fn swap_query(
    query: &str,
    entries: &[Arc<StandIn>],
    host: &Host,
    report: &mut Swapped,
) -> Option<Zeroizing<Vec<u8>>> {
    let params: Vec<Param<'_>> = query.split('&').map(Param::new).collect();
    let mut given: HashMap<&[u8], usize> = HashMap::new();
    for param in &params {
        *given.entry(&param.name).or_default() += 1;
    }
    let swaps: Vec<Option<Zeroizing<Vec<u8>>>> = params
        .iter()
        .map(|param| {
            let value = decoded(param.value?);
            let entry = entries.iter().find(|e| e.text.as_bytes() == &*value)?;
            let Placement::Query(placed) = &entry.placement else {
                return None;
            };
            let is_placed = *param.name == *placed.as_str().as_bytes();
            let once = given.get(placed.as_str().as_bytes()) == Some(&1);
            let swaps = is_placed && once && entry.hosts.contains(host);
            swaps.then(|| {
                Swapped::note(&mut report.swapped, entry.id());
                encoded(entry.real.expose(), keeps_in_query)
            })
        })
        .collect();
    if swaps.iter().all(Option::is_none) {
        return None;
    }
    let mut out = Zeroizing::new(Vec::with_capacity(query.len()));
    for (at, (param, new)) in params.iter().zip(&swaps).enumerate() {
        if at > 0 {
            out.push(b'&');
        }
        out.extend_from_slice(param.sent.as_bytes());
        let value = new
            .as_deref()
            .map(Vec::as_slice)
            .or(param.value.map(str::as_bytes));
        if let Some(value) = value {
            out.push(b'=');
            out.extend_from_slice(value);
        }
    }
    Some(out)
}

/// One `name=value` (or bare `name`) of a query.
struct Param<'q> {
    /// The name as sent.
    sent: &'q str,
    /// The name with its escapes decoded, as a server compares it.
    name: Cow<'q, [u8]>,
    /// The value as sent.
    value: Option<&'q str>,
}

impl<'q> Param<'q> {
    fn new(text: &'q str) -> Self {
        let (sent, value) = match text.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (text, None),
        };
        Self {
            sent,
            name: decoded(sent),
            value,
        }
    }
}

fn decoded(text: &str) -> Cow<'_, [u8]> {
    percent_decoded(text.as_bytes()).map_or(Cow::Borrowed(text.as_bytes()), Cow::Owned)
}

/// A path character that needs no escape: RFC 3986 `pchar` and `/` (a value may span segments,
/// as some webhook secrets do), less `%` and `;` (some servers cut a segment at `;`).
pub(super) fn keeps_in_path(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,=:@/".contains(&byte)
}

/// A query value character that needs no escape and that no server reads as a separator or a
/// space (so not `&`, `=`, `+`, `#`, nor `;`, which some servers split on too).
pub(super) fn keeps_in_query(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$'()*,:@/?".contains(&byte)
}

/// `real` with every byte `keeps` refuses percent-encoded, except a `%` that already starts an
/// escape (`%2B`): a value copied from a URL is often encoded already, and encoding it again
/// would change it.
pub(super) fn encoded(real: &str, keeps: fn(u8) -> bool) -> Zeroizing<Vec<u8>> {
    let bytes = real.as_bytes();
    let mut out = Zeroizing::new(Vec::with_capacity(bytes.len()));
    for (at, &byte) in bytes.iter().enumerate() {
        let escape = byte == b'%'
            && bytes
                .get(at + 1..at + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit));
        if escape || keeps(byte) {
            out.push(byte);
        } else {
            // Written digit by digit: a formatted string would be a copy of the real byte that
            // is never wiped.
            let hex = |nibble: u8| match nibble {
                0..=9 => b'0' + nibble,
                _ => b'A' + nibble - 10,
            };
            out.extend_from_slice(&[b'%', hex(byte >> 4), hex(byte & 0xF)]);
        }
    }
    out
}

#[cfg(test)]
mod tests;
