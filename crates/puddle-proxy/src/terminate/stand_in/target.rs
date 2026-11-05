// SPDX-License-Identifier: GPL-3.0-or-later
//! The request target: where the opt-in [`Placement::Query`] and [`Placement::Path`] stand-ins
//! are swapped, and how the target is written down for the audit.
//!
//! The target is origin form (`/path?query`) by the time it gets here: both the HTTP/1.1 parser
//! and the HTTP/2 check refuse anything else, and neither lets a fragment through.

use std::borrow::Cow;
use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError};

use ::http::{HeaderMap, HeaderValue};
use memchr::memmem::Finder;

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

    /// Puts the stand-in back wherever the headers of an answer from `host` repeat the real value
    /// of a secret placed in the path or a query parameter for that host, as sent or as Puddle
    /// encoded it: servers echo the URL they got, in a redirect's `Location` above all, and the
    /// guest must not read the real value there. A header secret is not looked for (a server
    /// rarely echoes one; see the accepted risk of echoes in the credentials spec), and neither is
    /// the body.
    pub(crate) fn hide_in_answer(&self, headers: &mut HeaderMap, host: &Host) {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let mut forms: Vec<(Zeroizing<Vec<u8>>, &str)> = Vec::new();
        for entry in guard.iter() {
            if entry.placement == Placement::Header || !entry.hosts.contains(host) {
                continue;
            }
            let real = entry.real.expose();
            for form in [
                Zeroizing::new(real.as_bytes().to_vec()),
                encoded(real, keeps_in_path),
                encoded(real, keeps_in_query),
            ] {
                if !form.is_empty() && !forms.iter().any(|(known, _)| *known == form) {
                    forms.push((form, &entry.text));
                }
            }
        }
        if forms.is_empty() {
            return;
        }
        // The longest form first, so a value that holds another is put back whole.
        forms.sort_by_key(|(form, _)| Reverse(form.len()));
        for value in headers.values_mut() {
            let mut text = Zeroizing::new(value.as_bytes().to_vec());
            let mut changed = false;
            for (form, stand_in) in &forms {
                if let Some(new) = replace_all(&text, form, stand_in.as_bytes()) {
                    text = new;
                    changed = true;
                }
            }
            if changed {
                // The stand-in is visible ASCII, so the value stays a valid header value; were it
                // not, the header would go empty rather than carry the real value.
                *value = HeaderValue::from_bytes(&text).unwrap_or(HeaderValue::from_static(""));
            }
        }
    }
}

/// `input` with every `needle` replaced by `with`, or `None` when it has none.
fn replace_all(input: &[u8], needle: &[u8], with: &[u8]) -> Option<Zeroizing<Vec<u8>>> {
    let finder = Finder::new(needle);
    let mut out: Option<Zeroizing<Vec<u8>>> = None;
    let mut copied = 0;
    while let Some(found) = input.get(copied..).and_then(|rest| finder.find(rest)) {
        let out = out.get_or_insert_with(|| Zeroizing::new(Vec::with_capacity(input.len())));
        out.extend_from_slice(input.get(copied..copied + found)?);
        out.extend_from_slice(with);
        copied += found + needle.len();
    }
    let mut out = out?;
    out.extend_from_slice(input.get(copied..)?);
    Some(out)
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
fn keeps_in_path(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$&'()*+,=:@/".contains(&byte)
}

/// A query value character that needs no escape and that no server reads as a separator or a
/// space (so not `&`, `=`, `+`, `#`, nor `;`, which some servers split on too).
fn keeps_in_query(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-._~!$'()*,:@/?".contains(&byte)
}

/// `real` with every byte `keeps` refuses percent-encoded, except a `%` that already starts an
/// escape (`%2B`): a value copied from a URL is often encoded already, and encoding it again
/// would change it.
fn encoded(real: &str, keeps: fn(u8) -> bool) -> Zeroizing<Vec<u8>> {
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
mod tests {
    use super::super::{SecretValue, StandInOrigin, TerminationSet, TokenBody};
    use super::*;
    use puddle_types::QueryName;

    const KEY: &str = "puddle-secret-OWM_KEY-00112233445566778899aabbccddeeff";
    const BOT: &str = "puddle-secret-BOT_TOKEN-ffeeddccbbaa99887766554433221100";
    const HDR: &str = "puddle-secret-API_KEY-0123456789abcdef0123456789abcdef";
    const REAL_KEY: &str = "owm-real-1111";
    const REAL_BOT: &str = "123456:ABC-real";
    const REAL_HDR: &str = "sk-real-2222";

    fn host(name: &str) -> Host {
        Host::parse_normalised(name).unwrap()
    }

    fn entry(name: &str, text: &str, real: &str, hosts: &[&str], placement: Placement) -> StandIn {
        StandIn::new(
            StandInOrigin::Secret,
            name,
            text,
            SecretValue::new(real),
            TerminationSet::parse(hosts.iter().copied()).unwrap(),
        )
        .unwrap()
        .placed(placement)
    }

    /// `OWM_KEY` in the query parameter `appid` toward `api.openweathermap.org`, `BOT_TOKEN` in
    /// the path toward `api.telegram.org`, `API_KEY` in headers toward `api.example.com`.
    fn registry() -> StandIns {
        let registry = StandIns::new();
        for entry in [
            entry(
                "OWM_KEY",
                KEY,
                REAL_KEY,
                &["api.openweathermap.org"],
                Placement::Query(QueryName::new("appid").unwrap()),
            ),
            entry(
                "BOT_TOKEN",
                BOT,
                REAL_BOT,
                &["api.telegram.org"],
                Placement::Path,
            ),
            entry(
                "API_KEY",
                HDR,
                REAL_HDR,
                &["api.example.com"],
                Placement::Header,
            ),
        ] {
            registry.insert(entry).unwrap();
        }
        registry
    }

    fn swap(registry: &StandIns, target: &str, to: &str) -> (Option<String>, Swapped) {
        let (new, report) = registry.swap_target(target, &host(to));
        (new.map(|new| new.as_str().to_owned()), report)
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter()
            .map(|name| format!("stand-in:secret:{name}"))
            .collect()
    }

    #[test]
    fn a_query_stand_in_is_swapped_in_its_named_parameter_toward_its_hosts() {
        let registry = registry();
        let (new, report) = swap(
            &registry,
            &format!("/data/3.0/onecall?lat=1&appid={KEY}&units=metric"),
            "api.openweathermap.org",
        );
        assert_eq!(
            new.as_deref(),
            Some(format!("/data/3.0/onecall?lat=1&appid={REAL_KEY}&units=metric").as_str())
        );
        assert_eq!(report.swapped, ids(&["OWM_KEY"]));
        assert_eq!(report.unswapped, Vec::<String>::new());

        // The name and the value are read as a server reads them, escapes decoded.
        let escaped = KEY.replacen('-', "%2d", 2);
        let (new, report) = swap(
            &registry,
            &format!("/x?app%69d={escaped}"),
            "api.openweathermap.org",
        );
        assert_eq!(
            new.as_deref(),
            Some(format!("/x?app%69d={REAL_KEY}").as_str())
        );
        assert_eq!(report.swapped, ids(&["OWM_KEY"]));
    }

    #[test]
    fn a_query_stand_in_anywhere_else_goes_out_unchanged_and_is_reported() {
        let registry = registry();
        for (target, to) in [
            // Toward a host it is not for.
            (format!("/x?appid={KEY}"), "evil.example.com"),
            (format!("/x?appid={KEY}"), "api.telegram.org"),
            // In another parameter, part of a value, a name, or the path.
            (format!("/x?key={KEY}"), "api.openweathermap.org"),
            (format!("/x?appid={KEY}x"), "api.openweathermap.org"),
            (format!("/x?appid=a{KEY}"), "api.openweathermap.org"),
            (format!("/x?{KEY}=appid"), "api.openweathermap.org"),
            (format!("/x?appid&{KEY}"), "api.openweathermap.org"),
            (format!("/{KEY}?appid=1"), "api.openweathermap.org"),
            // The named parameter given twice.
            (
                format!("/x?appid={KEY}&appid={KEY}"),
                "api.openweathermap.org",
            ),
            (
                format!("/x?appid={KEY}&appid=other"),
                "api.openweathermap.org",
            ),
            // A name that only looks alike.
            (format!("/x?APPID={KEY}"), "api.openweathermap.org"),
            (format!("/x?appid%20={KEY}"), "api.openweathermap.org"),
        ] {
            let (new, report) = swap(&registry, &target, to);
            assert_eq!(new, None, "{target} to {to}");
            assert_eq!(report.swapped, Vec::<String>::new(), "{target} to {to}");
            assert_eq!(report.unswapped, ids(&["OWM_KEY"]), "{target} to {to}");
        }
    }

    #[test]
    fn a_path_stand_in_is_swapped_in_the_path_toward_its_hosts_and_nowhere_else() {
        let registry = registry();
        let (new, report) = swap(
            &registry,
            &format!("/bot{BOT}/sendMessage?chat_id=1"),
            "api.telegram.org",
        );
        assert_eq!(
            new.as_deref(),
            Some(format!("/bot{REAL_BOT}/sendMessage?chat_id=1").as_str())
        );
        assert_eq!(report.swapped, ids(&["BOT_TOKEN"]));
        assert_eq!(report.unswapped, Vec::<String>::new());

        // A whole segment, twice.
        let (new, _) = swap(&registry, &format!("/{BOT}/{BOT}"), "api.telegram.org");
        assert_eq!(
            new.as_deref(),
            Some(format!("/{REAL_BOT}/{REAL_BOT}").as_str())
        );

        for (target, to) in [
            (format!("/bot{BOT}/getMe"), "evil.example.com"),
            (format!("/getMe?token={BOT}"), "api.telegram.org"),
            (format!("/getMe?{BOT}"), "api.telegram.org"),
            // Escaped, it is not what the guest's tool would send: left, and reported.
            (
                format!("/bot{}/getMe", BOT.replacen('-', "%2D", 1)),
                "api.telegram.org",
            ),
        ] {
            let (new, report) = swap(&registry, &target, to);
            assert_eq!(new, None, "{target} to {to}");
            assert_eq!(report.unswapped, ids(&["BOT_TOKEN"]), "{target} to {to}");
        }
    }

    #[test]
    fn a_header_stand_in_in_the_target_is_never_swapped_and_is_reported() {
        let registry = registry();
        for target in [
            format!("/a/{HDR}"),
            format!("/a?appid={HDR}"),
            format!("/a?key={HDR}"),
        ] {
            let (new, report) = swap(&registry, &target, "api.example.com");
            assert_eq!(new, None, "{target}");
            assert_eq!(report.unswapped, ids(&["API_KEY"]), "{target}");
        }
    }

    #[test]
    fn a_swap_in_one_place_still_reports_a_copy_left_in_another() {
        let registry = registry();
        let (new, report) = swap(
            &registry,
            &format!("/bot{BOT}/x?appid={KEY}&copy={BOT}&h={HDR}"),
            "api.telegram.org",
        );
        assert_eq!(
            new.as_deref(),
            Some(format!("/bot{REAL_BOT}/x?appid={KEY}&copy={BOT}&h={HDR}").as_str())
        );
        assert_eq!(report.swapped, ids(&["BOT_TOKEN"]));
        assert_eq!(report.unswapped, ids(&["OWM_KEY", "BOT_TOKEN", "API_KEY"]));
    }

    #[test]
    fn a_real_value_is_encoded_for_its_place_and_an_escape_in_it_is_kept() {
        let registry = StandIns::new();
        registry
            .insert(entry(
                "Q",
                KEY,
                "a b&c=d+e#f/g?h%2Bi%zz;",
                &["q.example.com"],
                Placement::Query(QueryName::new("sig").unwrap()),
            ))
            .unwrap();
        registry
            .insert(entry(
                "P",
                BOT,
                "a b/c?d#e%2Fx:y@z&;",
                &["q.example.com"],
                Placement::Path,
            ))
            .unwrap();
        let (new, _) = swap(&registry, &format!("/{BOT}?sig={KEY}"), "q.example.com");
        assert_eq!(
            new.as_deref(),
            Some("/a%20b/c%3Fd%23e%2Fx:y@z&%3B?sig=a%20b%26c%3Dd%2Be%23f/g?h%2Bi%25zz%3B")
        );
    }

    #[test]
    fn a_query_parameter_without_a_value_and_an_empty_query_are_kept_as_sent() {
        let registry = registry();
        let (new, _) = swap(
            &registry,
            &format!("/x?flag&appid={KEY}&&end="),
            "api.openweathermap.org",
        );
        assert_eq!(
            new.as_deref(),
            Some(format!("/x?flag&appid={REAL_KEY}&&end=").as_str())
        );
        assert_eq!(
            swap(&registry, "/x?", "api.openweathermap.org"),
            (None, Swapped::default())
        );
        assert_eq!(
            swap(&registry, "/x", "api.telegram.org"),
            (None, Swapped::default())
        );
        assert_eq!(
            swap(&StandIns::new(), &format!("/bot{BOT}"), "api.telegram.org"),
            (None, Swapped::default())
        );
    }

    #[test]
    fn a_target_stand_in_is_not_swapped_in_headers_or_a_refresh_field() {
        let registry = registry();
        let mut headers = ::http::HeaderMap::new();
        headers.insert("x-api-key", KEY.parse().unwrap());
        headers.insert("authorization", format!("Bearer {BOT}").parse().unwrap());
        let report = registry.swap(&mut headers, &host("api.openweathermap.org"), None);
        assert_eq!(headers["x-api-key"], KEY);
        assert_eq!(report.unswapped, ids(&["OWM_KEY", "BOT_TOKEN"]));
        assert_eq!(report.swapped, Vec::<String>::new());
        let report = registry.swap(&mut headers, &host("api.telegram.org"), None);
        assert_eq!(headers["authorization"], format!("Bearer {BOT}"));
        assert_eq!(report.swapped, Vec::<String>::new());

        let mut body = TokenBody::parse(
            Some("application/x-www-form-urlencoded"),
            format!("refresh_token={KEY}").as_bytes(),
        )
        .unwrap();
        let (report, changed) = registry.swap_fields(
            &mut body,
            &["refresh_token".to_owned()],
            &host("api.openweathermap.org"),
        );
        assert!(!changed);
        assert_eq!(report.unswapped, ids(&["OWM_KEY"]));
    }

    #[test]
    fn the_audit_target_has_a_label_in_place_of_each_stand_in() {
        let registry = registry();
        assert_eq!(
            registry.redact_target(&format!("/bot{BOT}/x?appid={KEY}")),
            "/bot[secret:BOT_TOKEN]/x?appid=[secret:OWM_KEY]"
        );
        let escaped = format!("/a/{}", HDR.replacen('-', "%2D", 1));
        assert_eq!(registry.redact_target(&escaped), "/a/[secret:API_KEY]");
        assert!(matches!(
            registry.redact_target("/plain?x=%41"),
            Cow::Borrowed("/plain?x=%41")
        ));
        assert!(matches!(
            StandIns::new().redact_target(&format!("/{BOT}")),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn hostile_the_audit_target_keeps_every_escape_but_the_stand_ins_own() {
        let registry = registry();
        // Decoding the whole line would put a line break and a `?` in the record.
        let escaped = HDR.replacen('-', "%2D", 1);
        assert_eq!(
            registry.redact_target(&format!("/x%0d%0a%3Fy/{escaped}/a%2Fb?q={BOT}&r=%41")),
            "/x%0d%0a%3Fy/[secret:API_KEY]/a%2Fb?q=[secret:BOT_TOKEN]&r=%41"
        );
        // Overlapping copies: each label once, nothing of either stand-in left.
        assert_eq!(
            registry.redact_target(&format!("/{HDR}{escaped}")),
            "/[secret:API_KEY][secret:API_KEY]"
        );
    }

    #[test]
    fn overlapping_stand_ins_in_the_audit_target_get_the_first_label() {
        // The end of one is the start of the other: neither contains the other, so both register.
        let registry = StandIns::new();
        for (name, text) in [
            ("FIRST", "prefix-aaaaaaaa-ZZZZ12345678"),
            ("SECOND", "ZZZZ12345678-bbbbbbbbbbbb"),
        ] {
            registry
                .insert(entry(
                    name,
                    text,
                    "real",
                    &["q.example.com"],
                    Placement::Path,
                ))
                .unwrap();
        }
        assert_eq!(
            registry.redact_target("/prefix-aaaaaaaa-ZZZZ12345678-bbbbbbbbbbbb/x"),
            "/[secret:FIRST]-bbbbbbbbbbbb/x"
        );
    }

    #[test]
    fn an_answer_header_that_repeats_a_target_secret_gets_the_stand_in_back() {
        let registry = StandIns::new();
        registry
            .insert(entry(
                "P",
                BOT,
                "a b/c",
                &["q.example.com"],
                Placement::Path,
            ))
            .unwrap();
        registry
            .insert(entry(
                "Q",
                KEY,
                "k&1",
                &["q.example.com"],
                Placement::Query(QueryName::new("sig").unwrap()),
            ))
            .unwrap();
        registry
            .insert(entry(
                "H",
                HDR,
                REAL_HDR,
                &["q.example.com"],
                Placement::Header,
            ))
            .unwrap();
        let mut headers = ::http::HeaderMap::new();
        let sent = "/a%20b/c/x?sig=k%261";
        headers.insert(
            "location",
            format!("https://q.example.com{sent}/").parse().unwrap(),
        );
        headers.insert("x-raw", "a b/c and k&1 and k%261".parse().unwrap());
        headers.insert("x-header-secret", REAL_HDR.parse().unwrap());
        headers.insert("x-plain", "nothing".parse().unwrap());
        let mut elsewhere = headers.clone();

        registry.hide_in_answer(&mut headers, &host("q.example.com"));
        assert_eq!(
            headers["location"],
            format!("https://q.example.com/{BOT}/x?sig={KEY}/")
        );
        assert_eq!(headers["x-raw"], format!("{BOT} and {KEY} and {KEY}"));
        assert_eq!(headers["x-header-secret"], REAL_HDR, "not looked for");
        assert_eq!(headers["x-plain"], "nothing");

        // Toward another host the secrets were never sent, so nothing is changed.
        let before = elsewhere.clone();
        registry.hide_in_answer(&mut elsewhere, &host("other.example.com"));
        assert_eq!(elsewhere, before);
        StandIns::new().hide_in_answer(&mut elsewhere, &host("q.example.com"));
        assert_eq!(elsewhere, before);
    }

    #[test]
    fn a_placement_is_the_header_unless_told_otherwise() {
        assert_eq!(Placement::default(), Placement::Header);
    }

    mod properties {
        use proptest::prelude::*;

        use super::*;

        proptest! {
            #[test]
            fn any_target_never_panics_and_without_a_stand_in_never_changes(
                target in "/[ -~]{0,80}",
            ) {
                let registry = registry();
                for to in ["api.openweathermap.org", "api.telegram.org"] {
                    let (new, report) = swap(&registry, &target, to);
                    prop_assert_eq!(new, None);
                    prop_assert_eq!(report, Swapped::default());
                }
                prop_assert_eq!(registry.redact_target(&target), target.as_str());
            }

            #[test]
            fn a_swapped_target_never_holds_a_stand_in_for_its_host_in_its_place(
                before in "[a-z/]{0,10}",
                after in "[a-z=&]{0,10}",
            ) {
                let registry = registry();
                let target = format!("/{before}bot{BOT}/x?{after}&appid={KEY}");
                let (new, _) = swap(&registry, &target, "api.telegram.org");
                let new = new.unwrap();
                prop_assert!(!new.contains(BOT) && new.contains(REAL_BOT));
                prop_assert!(new.contains(KEY), "a query stand-in is not for this host");
            }
        }
    }
}
