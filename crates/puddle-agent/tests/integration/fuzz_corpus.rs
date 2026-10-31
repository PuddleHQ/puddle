// SPDX-License-Identifier: GPL-3.0-or-later
//! Replays the checked-in corpus of the `dns_message` cargo-fuzz target (`fuzz/corpus/dns_message/`)
//! through the same entry point on the stable toolchain, so a plain test run keeps every seed and
//! every input a nightly fuzz run once crashed on passing (see `fuzz/README.md`).
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::Path;

use puddle_agent::capture::wire::{self, QueryName, Reject};

// The same checks the cargo-fuzz target runs.
#[path = "../../../../fuzz/checks/dns_message.rs"]
mod fuzzing;

fn corpus() -> Vec<(String, Vec<u8>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/dns_message");
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
fn every_dns_message_in_the_corpus_keeps_the_answer_invariants() {
    for (_, bytes) in corpus() {
        fuzzing::dns_message(&bytes);
    }
}

#[test]
fn the_corpus_holds_messages_the_stub_answers_drops_and_refuses_by_name() {
    let inputs = corpus();
    let parse = |name: &str| wire::parse_query(&inputs.iter().find(|(n, _)| n == name).unwrap().1);
    assert_eq!(
        parse("a-query").unwrap().name,
        QueryName::Plain("example.com".into())
    );
    assert_eq!(parse("odd-escape-dot").unwrap().name, QueryName::Odd);
    assert_eq!(parse("edns-over-limit").unwrap().edns_payload, Some(65535));
    for dropped in ["response-bit", "empty", "one-byte", "header-11-bytes"] {
        assert_eq!(parse(dropped).unwrap_err(), Reject::Drop, "{dropped}");
    }
    for refused in [
        "compression-pointer",
        "label-64",
        "name-over-255",
        "name-no-root",
        "two-questions",
        "no-question",
        "answer-count",
        "authority-count",
        "qtype-cut-short",
        "header-only",
    ] {
        assert!(
            matches!(parse(refused), Err(Reject::Answer { .. })),
            "{refused}"
        );
    }
}
