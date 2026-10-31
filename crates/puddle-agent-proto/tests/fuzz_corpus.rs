// SPDX-License-Identifier: GPL-3.0-or-later
//! Replays the checked-in corpus of the `resolve_line` cargo-fuzz target (`fuzz/corpus/resolve_line/`)
//! through the same entry point on the stable toolchain, so a plain test run keeps every seed and
//! every input a nightly fuzz run once crashed on passing (see `fuzz/README.md`).
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::Path;

use puddle_agent_proto::fuzzing;
use puddle_agent_proto::resolve::{RecordType, ResolveAnswer, ResolveQuery};

fn corpus() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/resolve_line");
    let mut inputs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(&path).unwrap())
        })
        .collect();
    assert!(!inputs.is_empty(), "{} has no inputs", dir.display());
    inputs.sort();
    inputs
}

#[test]
fn every_resolve_line_in_the_corpus_keeps_the_line_invariants() {
    for (_, bytes) in corpus() {
        fuzzing::resolve_line(&bytes);
    }
}

#[test]
fn the_corpus_holds_lines_the_host_takes_and_lines_it_refuses_by_name() {
    let inputs = corpus();
    let bytes = |name: &str| &inputs.iter().find(|(n, _)| n == name).unwrap().1;
    assert_eq!(
        ResolveQuery::from_line(bytes("a-query")).unwrap(),
        ResolveQuery::new("example.com", RecordType::A)
    );
    for refused in [
        "type-aaaa",
        "type-lower-case",
        "no-type",
        "no-name",
        "name-number",
        "name-null",
        "name-invalid-utf8",
        "two-lines",
        "array",
        "empty",
        "truncated",
        "deep-nesting",
    ] {
        assert!(
            ResolveQuery::from_line(bytes(refused)).is_err(),
            "{refused} was taken as a query"
        );
    }
    assert!(ResolveAnswer::from_line(bytes("answer-records")).is_ok());
    assert_eq!(
        ResolveAnswer::from_line(bytes("answer-from-the-future")).unwrap(),
        ResolveAnswer::Unknown
    );
    assert!(ResolveAnswer::from_line(bytes("answer-ttl-negative")).is_err());
}
