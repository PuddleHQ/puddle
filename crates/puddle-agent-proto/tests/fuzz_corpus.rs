// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replays the checked-in corpus of the `resolve_line` cargo-fuzz target (`fuzz/corpus/resolve_line/`)
//! through the same entry point on the stable toolchain, so a plain test run keeps every seed and
//! every input a nightly fuzz run once crashed on passing (see `fuzz/README.md`).
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::Path;

use puddle_agent_proto::fuzzing::{self, LineGrammar};
use puddle_agent_proto::resolve::{RecordType, ResolveAnswer, ResolveQuery};

fn corpus() -> Vec<(String, Vec<u8>)> {
    corpus_of("resolve_line")
}

fn corpus_of(target: &str) -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/corpus")
        .join(target);
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

#[test]
fn every_grammar_line_in_the_corpus_keeps_the_grammar_invariants() {
    for (name, input) in corpus_of("resolve_line_grammar") {
        let line = LineGrammar::from_input(&input);
        assert!(line.is_some(), "{name} does not decode");
        if let Some(line) = line {
            fuzzing::resolve_line_grammar(&line);
        }
    }
}

#[test]
fn the_grammar_corpus_reaches_every_shape_of_line_the_grammar_makes() {
    let mut shapes: Vec<&str> = corpus_of("resolve_line_grammar")
        .iter()
        .filter_map(|(_, input)| LineGrammar::from_input(input).map(|l| l.shape()))
        .collect();
    shapes.sort_unstable();
    shapes.dedup();
    assert_eq!(shapes, ["answer", "not a query", "not an answer", "query"]);
}

#[test]
fn every_exchange_in_the_ask_corpus_keeps_the_stream_invariants() {
    for (name, input) in corpus_of("resolve_ask") {
        let exchange = fuzzing::AskExchange::from_input(&input);
        assert!(exchange.is_some(), "{name} does not decode");
        if let Some(exchange) = exchange {
            fuzzing::resolve_ask(&exchange);
        }
    }
}

#[test]
fn the_ask_corpus_reaches_every_shape_of_reply_the_grammar_makes() {
    let mut shapes: Vec<&str> = corpus_of("resolve_ask")
        .iter()
        .filter_map(|(_, input)| fuzzing::AskExchange::from_input(input).map(|e| e.shape()))
        .collect();
    shapes.sort_unstable();
    shapes.dedup();
    assert_eq!(shapes, ["answer", "cut", "junk"]);
}
