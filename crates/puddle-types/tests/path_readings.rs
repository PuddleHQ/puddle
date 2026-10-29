// SPDX-License-Identifier: GPL-3.0-or-later
//! A differential test of the paths a rule on method and path compares against the paths real
//! servers read. Each `view` below models one family of servers; for any request target built
//! from hostile pieces, a pattern that names the path a server reads must also name one of
//! [`CanonicalPath::readings`] of that target. Otherwise a rule written for the server's path
//! would let the request through.
#![expect(
    clippy::unwrap_used,
    reason = "a helper outside #[test] fails the test by panicking"
)]

use proptest::prelude::*;
use puddle_types::{CanonicalPath, PathPattern};

/// One round of `%XX` decoding, as nearly every server does it.
fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let hex = |i: usize| {
            bytes
                .get(i)
                .and_then(|b| char::from(*b).to_digit(16))
                .and_then(|d| u8::try_from(d).ok())
        };
        if byte == b'%'
            && let (Some(high), Some(low)) = (hex(at + 1), hex(at + 2))
        {
            out.push(high * 16 + low);
            at += 3;
        } else {
            out.push(byte);
            at += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `path` split at `seps`, with `.` and empty segments dropped and `..` taking one away.
fn resolve(path: &str, seps: &[char]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for segment in path.split(seps) {
        match segment {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            name => out.push(name.to_owned()),
        }
    }
    out
}

/// The target without query and fragment, as written.
fn raw_path(target: &str) -> &str {
    target.split(['?', '#']).next().unwrap_or_default()
}

/// The path segments servers of each family route on.
fn views(target: &str) -> Vec<(&'static str, Vec<String>)> {
    let raw = raw_path(target);
    let mut views = vec![
        // nginx, WSGI `PATH_INFO`: decode once, then resolve.
        ("decode first", resolve(&decode(raw), &['/'])),
        // A decoding hop that passes the path on: decode twice.
        ("decode twice", resolve(&decode(&decode(raw)), &['/'])),
        // A router that matches undecoded segments and decodes each parameter (Express, Rails).
        (
            "raw segments",
            resolve(raw, &['/']).iter().map(|s| decode(s)).collect(),
        ),
        // A hop that decodes and sends the result on, read again: a decoded `?` or `#` ends it.
        (
            "decode and read again",
            resolve(
                decode(raw).split(['?', '#']).next().unwrap_or_default(),
                &['/'],
            ),
        ),
        // Windows servers: `\` is a separator, trailing dots and spaces go.
        (
            "windows",
            resolve(
                &decode(raw)
                    .split(['/', '\\'])
                    .map(|s| {
                        let s = s.trim_end_matches(' ');
                        if s == "." || s == ".." {
                            s
                        } else {
                            s.trim_end_matches(['.', ' '])
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("/"),
                &['/'],
            ),
        ),
    ];
    // The WHATWG URL parser, resolving the target against the server's own address (Node's
    // `new URL(req.url, base)`, Deno, Bun): `\` is `/`, `%2e` is a dot, and `//name/` is an
    // authority. Each segment is then decoded once.
    let base = url::Url::parse("http://server.example/").unwrap();
    if let Ok(joined) = base.join(target) {
        views.push((
            "whatwg",
            joined
                .path()
                .split('/')
                .filter(|s| !s.is_empty())
                .map(decode)
                .collect(),
        ));
    }
    views
}

/// The pieces a hostile target is built from.
const PIECES: &[&str] = &[
    "admin", "ADMIN", "a", "x", ".", "..", "%2e", "%2E%2e", ".%2e", "/", "//", "\\", "%2f", "%5c",
    "%252f", "%25", "%3f", "%23", "?", "%00", "%20", ";p=1", "%41", "%u0061", "...",
];

fn target() -> impl Strategy<Value = String> {
    proptest::collection::vec(proptest::sample::select(PIECES), 0..8)
        .prop_map(|pieces| format!("/{}", pieces.concat()))
}

/// A plain name: letters, digits, `-` and `_`.
fn plain(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A segment that can name a real resource: plain names, joined by `/` when an API takes an
/// escaped path as one segment (`/projects/group%2Fproject`). A rule protects real resources,
/// so a segment a server reads as `;p=1\..` or ` .` is no way around one.
fn real(segment: &str) -> bool {
    segment.split('/').all(plain)
}

/// A pattern that names `segments` on a server: each segment as itself (when it is a plain
/// name, by `mask`) or `*`, with a closing `/**` after the first `open_at` segments. `None` when
/// a segment the pattern covers is not [`real`].
fn patterns(segments: &[String], mask: u32, open_at: Option<usize>) -> Option<String> {
    let end = open_at.map_or(segments.len(), |at| at.min(segments.len()));
    if !segments.iter().take(end).all(|s| real(s)) {
        return None;
    }
    let mut text = String::new();
    for (at, segment) in segments.iter().take(end).enumerate() {
        text.push('/');
        if mask & (1 << (at % 32)) != 0 && plain(segment) {
            text.push_str(segment);
        } else {
            text.push('*');
        }
    }
    if open_at.is_some() {
        text.push_str("/**");
    }
    if text.is_empty() {
        text.push('/');
    }
    Some(text)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(4096))]

    #[test]
    fn a_pattern_that_names_what_a_server_reads_names_a_reading(
        target in target(),
        mask in any::<u32>(),
        open_at in proptest::option::of(0_usize..8),
    ) {
        let readings = CanonicalPath::readings(&target);
        for (view, segments) in views(&target) {
            // A `%` a server leaves standing is an escape it did not decode; a rule decodes
            // further, so that view names no real resource here.
            if segments.iter().any(|s| s.contains('%')) {
                continue;
            }
            let Some(text) = patterns(&segments, mask, open_at) else { continue };
            let Ok(pattern) = PathPattern::parse(&text) else { continue };
            prop_assert!(
                readings.iter().any(|reading| pattern.matches(reading)),
                "{view} reads {target:?} as {segments:?}; {text} names it, but no reading: {:?}",
                readings.iter().map(CanonicalPath::segments).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn the_readings_cover_the_known_server_disagreements() {
    let named = |pattern: &str, target: &str| {
        let pattern = PathPattern::parse(pattern).unwrap();
        CanonicalPath::readings(target)
            .iter()
            .any(|reading| pattern.matches(reading))
    };
    assert!(named("/repos/*", "/repos/a%2fb"));
    assert!(named("/admin", "/admin%3fx=1"));
    assert!(named("/admin", "//evil.example/admin"));
    assert!(named("/admin", "/public#/../admin"));
    // A space before trailing dots keeps the segment for a server that is not Windows, so a
    // `*` still takes it there.
    assert!(named("/x/*/y", "/x/%20./y"));
    assert!(named("/x/*/y", "/x/%20.%20./y"));
    assert!(named("/admin", "/admin%20."));
    assert!(!named("/admin", "/administrator"));
    assert!(!named("/repos/*", "/repos/a/b"));
}
