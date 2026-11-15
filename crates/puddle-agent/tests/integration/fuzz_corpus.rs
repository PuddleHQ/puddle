// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replays the checked-in corpus of the `dns_message` cargo-fuzz target (`fuzz/corpus/dns_message/`)
//! through the same entry point on the stable toolchain, so a plain test run keeps every seed and
//! every input a nightly fuzz run once crashed on passing (see `fuzz/README.md`).
#![expect(
    clippy::unwrap_used,
    reason = "helpers outside #[test] functions fail the test by panicking"
)]

use std::path::Path;

use arbitrary::{Arbitrary, Unstructured};

use puddle_agent::capture::wire::{self, MAX_QUERY, QueryName, Reject};

// The same checks the cargo-fuzz target runs.
#[path = "../../../../fuzz/checks/dns_message.rs"]
mod dns_message;
#[path = "../../../../fuzz/checks/dns_message_grammar.rs"]
mod dns_message_grammar;
use dns_message_grammar::Message;
#[path = "../../../../fuzz/checks/dns_transport.rs"]
mod dns_transport;
use dns_transport::{Probe, Stream};

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
        dns_message::dns_message(&bytes);
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
        "crash-name-over-255-in-last-label",
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

fn grammar_corpus() -> Vec<(String, Option<Message>)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/dns_message_grammar");
    let mut inputs: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let bytes = std::fs::read(&path).unwrap();
            let message = Message::arbitrary(&mut Unstructured::new(&bytes));
            assert!(message.is_ok(), "{name} does not decode");
            (name, message.ok())
        })
        .collect();
    assert!(!inputs.is_empty(), "{} has no inputs", dir.display());
    inputs.sort_by(|a, b| a.0.cmp(&b.0));
    inputs
}

#[test]
fn every_grammar_message_in_the_corpus_keeps_the_grammar_invariants() {
    for message in grammar_corpus().iter().filter_map(|(_, m)| m.as_ref()) {
        dns_message_grammar::dns_message_grammar(message);
    }
}

#[test]
fn the_grammar_corpus_reaches_every_shape_of_message_the_grammar_makes() {
    let mut shapes: Vec<&str> = grammar_corpus()
        .iter()
        .filter_map(|(_, m)| m.as_ref().map(Message::shape))
        .collect();
    shapes.sort_unstable();
    shapes.dedup();
    assert_eq!(
        shapes,
        ["answer", "drop", "query", "unchecked"],
        "the corpus lacks a shape of message"
    );
}

fn transport_corpus<T: for<'a> Arbitrary<'a>>(target: &str) -> Vec<T> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/corpus")
        .join(target);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert!(!files.is_empty(), "{} has no inputs", dir.display());
    files.sort();
    files
        .iter()
        .map(|path| {
            let bytes = std::fs::read(path).unwrap();
            let input = T::arbitrary(&mut Unstructured::new(&bytes));
            assert!(input.is_ok(), "{} does not decode", path.display());
            input.unwrap()
        })
        .collect()
}

#[test]
fn every_datagram_in_the_udp_corpus_is_answered_as_the_stub_answers_it() {
    for probe in transport_corpus::<Probe>("dns_udp_datagram") {
        dns_transport::dns_udp_datagram(&probe);
    }
}

#[test]
fn a_datagram_one_byte_over_the_query_limit_is_dropped_and_the_next_is_answered() {
    dns_transport::dns_udp_bytes(&dns_transport::padded_query(MAX_QUERY + 1));
}

#[test]
fn a_datagram_twice_the_query_limit_is_dropped_and_the_next_is_answered() {
    dns_transport::dns_udp_bytes(&dns_transport::padded_query(2 * MAX_QUERY));
}

#[test]
fn a_query_padded_to_the_query_limit_is_a_query_the_stub_answers() {
    assert!(wire::parse_query(&dns_transport::padded_query(MAX_QUERY)).is_ok());
}

#[test]
fn a_datagram_at_the_query_limit_is_answered_as_the_stub_answers_it() {
    dns_transport::dns_udp_bytes(&dns_transport::padded_query(MAX_QUERY));
}

#[test]
fn the_udp_corpus_holds_every_kind_of_datagram_the_grammar_makes() {
    let mut shapes: Vec<&str> = transport_corpus::<Probe>("dns_udp_datagram")
        .iter()
        .map(Probe::shape)
        .collect();
    shapes.sort_unstable();
    shapes.dedup();
    assert_eq!(shapes, ["bounded", "grammar", "raw"]);
}

#[test]
fn every_stream_in_the_tcp_corpus_is_answered_as_the_framing_says() {
    for stream in transport_corpus::<Stream>("dns_tcp_stream") {
        dns_transport::dns_tcp_stream(&stream);
    }
}

#[test]
fn the_tcp_corpus_holds_every_way_a_frame_states_its_length() {
    let mut kinds: Vec<&str> = transport_corpus::<Stream>("dns_tcp_stream")
        .iter()
        .flat_map(Stream::kinds)
        .collect();
    kinds.sort_unstable();
    kinds.dedup();
    assert_eq!(
        kinds,
        [
            "cut",
            "honest",
            "limit",
            "over",
            "past the limit",
            "under",
            "zero"
        ]
    );
}

#[test]
fn a_header_cut_short_and_padded_is_judged_by_the_bytes_that_remain() {
    // The grammar wrote an authority count of 1, cut the message after the answer count and padded
    // it with zero bytes, so the stub reads a header with no authority records: a query.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fuzz/corpus/dns_message_grammar/crash-header-cut-and-padded");
    let message =
        Message::arbitrary(&mut Unstructured::new(&std::fs::read(path).unwrap())).unwrap();
    assert_eq!(message.shape(), "unchecked");
    assert_eq!(
        message.bytes(),
        [
            0x87, 0xfe, 0x03, 0xff, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0
        ]
    );
    let query = wire::parse_query(message.bytes()).unwrap();
    assert_eq!(
        (query.id, query.name),
        (0x87fe, QueryName::Plain(String::new()))
    );
    dns_message_grammar::dns_message_grammar(&message);
}
