// SPDX-License-Identifier: GPL-3.0-or-later
//! Replays the checked-in corpus of every cargo-fuzz target (`fuzz/corpus/<target>/`) through the
//! same entry points on the stable toolchain, so a plain test run keeps every seed and every
//! input a nightly fuzz run once crashed on (it is committed there as a regression, see
//! `fuzz/README.md`) passing.
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::{Path, PathBuf};

use puddle_proxy::fuzzing;

fn corpus(target: &str) -> Vec<(PathBuf, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/corpus")
        .join(target);
    let mut inputs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        })
        .collect();
    assert!(!inputs.is_empty(), "{} has no inputs", dir.display());
    inputs.sort();
    inputs
}

#[test]
fn the_head_parser_corpus_is_refused_or_accepted_without_breaking_a_limit() {
    for (_, bytes) in corpus("head_parser") {
        let _ = fuzzing::head(&bytes);
    }
    // Seeds that must reach the rules, and seeds that must not, by name.
    let outcome = |name: &str| {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../fuzz/corpus/head_parser")
                .join(name),
        )
        .unwrap();
        fuzzing::head(&bytes)
    };
    assert_eq!(
        outcome("connect-ok"),
        fuzzing::HeadOutcome::Target {
            host: "example.com".into(),
            port: 443
        }
    );
    for refused in [
        "nul-in-line",
        "non-utf8",
        "no-crlf",
        "http-9",
        "double-space",
    ] {
        assert_eq!(outcome(refused), fuzzing::HeadOutcome::Refused, "{refused}");
    }
    for refused in [
        "userinfo",
        "hex-ip",
        "port-zero",
        "port-too-big",
        "unclosed-bracket",
    ] {
        assert_eq!(
            outcome(refused),
            fuzzing::HeadOutcome::NotAProxyRequest,
            "{refused}"
        );
    }
}

/// Too big to keep in the corpus (it is over the 64 KiB head limit): made here.
#[test]
fn a_head_over_the_limit_is_too_large_not_a_target() {
    let big = [
        b"GET http://a.test/ HTTP/1.1\r\nX: ".to_vec(),
        vec![b'a'; 70_000],
    ]
    .concat();
    assert_eq!(fuzzing::head(&big), fuzzing::HeadOutcome::TooLarge);
}

#[test]
fn the_connect_target_corpus_keeps_every_target_invariant() {
    for (_, bytes) in corpus("connect_target") {
        let uri = std::str::from_utf8(&bytes).unwrap();
        let _ = fuzzing::target("CONNECT", uri);
        let _ = fuzzing::target("GET", uri);
    }
}

#[test]
fn the_host_name_corpus_normalises_to_stable_printable_ascii_or_is_refused() {
    for (_, bytes) in corpus("normalise_host") {
        fuzzing::host(&bytes);
    }
}

#[test]
fn the_agent_preamble_corpus_is_accepted_only_in_the_registered_shape() {
    for (_, bytes) in corpus("agent_preamble") {
        fuzzing::preamble(&bytes);
    }
}
