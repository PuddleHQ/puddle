// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checks of the `dns_message_grammar` cargo-fuzz target: what the stub must make of a message
//! built by `dns_message_builder.rs`. Included, beside `dns_message.rs` as `dns_message`, by the fuzz
//! target and by the stable corpus replay in `puddle-agent`'s tests.

use puddle_agent::capture::wire::{self, Rdata, Record, Reject};

use super::dns_message::dns_message;
use super::dns_message_builder::{Edns, Expect, Message, Reply};

/// Reads a grammar-built message and checks what the stub must make of it, then runs the
/// byte-level checks of [`dns_message`] on the same bytes.
///
/// # Panics
///
/// When the stub drops, refuses or reads a message differently from what its grammar says, when
/// the answer to a query does not carry the question as it was sent, or on anything
/// [`dns_message`] panics on.
pub fn dns_message_grammar(message: &Message) {
    let bytes = message.bytes();
    let parsed = wire::parse_query(bytes);
    match &message.expect {
        Expect::Drop => assert_eq!(
            parsed.err(),
            Some(Reject::Drop),
            "a message the stub must drop"
        ),
        Expect::Answer(rcode) => assert!(
            matches!(parsed, Err(Reject::Answer { rcode: got, .. }) if got == *rcode),
            "expected the code {rcode:?}, got {parsed:?}"
        ),
        Expect::Query {
            name,
            qtype,
            qclass,
            edns,
            question,
        } => {
            assert!(
                parsed.is_ok(),
                "a well-formed query was refused: {parsed:?}"
            );
            let Ok(query) = &parsed else { return };
            assert_eq!(&query.name, name, "the name");
            assert_eq!(
                (query.qtype, query.qclass),
                (*qtype, *qclass),
                "type and class"
            );
            match edns {
                Edns::Unchecked => {}
                Edns::Absent => {
                    assert_eq!(query.edns_payload, None, "an OPT record that is not there");
                }
                Edns::Size(size) => {
                    assert_eq!(
                        query.edns_payload,
                        Some(*size),
                        "the advertised payload size"
                    );
                }
            }
            check_reply(query, question, &message.reply);
        }
        Expect::Unchecked => {}
    }
    dns_message(bytes);
}

/// A cursor over the answer the stub wrote, for reading it back.
struct Wire<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Wire<'_> {
    fn take(&mut self, n: usize) -> &[u8] {
        let part = self.bytes.get(self.at..self.at + n).unwrap_or_default();
        assert_eq!(part.len(), n, "the answer ends inside a field");
        self.at += n;
        part
    }

    fn word(&mut self) -> u16 {
        let b = self.take(2);
        u16::from_be_bytes([
            b.first().copied().unwrap_or(0),
            b.get(1).copied().unwrap_or(0),
        ])
    }

    fn long(&mut self) -> u32 {
        let hi = u32::from(self.word());
        (hi << 16) | u32::from(self.word())
    }

    /// The labels of an uncompressed name.
    fn name(&mut self) -> Vec<Vec<u8>> {
        let mut labels = Vec::new();
        loop {
            let len = usize::from(self.take(1).first().copied().unwrap_or(0));
            assert!(len <= 63, "a label of {len} bytes in an answer");
            if len == 0 {
                return labels;
            }
            labels.push(self.take(len).to_vec());
        }
    }
}

fn labels_of(name: &str) -> Vec<Vec<u8>> {
    name.split('.')
        .filter(|l| !l.is_empty())
        .map(|l| l.as_bytes().to_vec())
        .collect()
}

/// Reads one record back and checks it says what `record` asked for.
fn check_record(wire: &mut Wire<'_>, record: &Record) {
    match &record.owner {
        None => assert_eq!(
            wire.take(2),
            [0xC0, 0x0C],
            "an owner that is the question's name"
        ),
        Some(owner) => assert_eq!(wire.name(), labels_of(owner), "the owner"),
    }
    let rtype = wire.word();
    assert_eq!(wire.word(), wire::CLASS_IN, "the class");
    assert_eq!(wire.long(), record.ttl, "the ttl");
    let rdlen = usize::from(wire.word());
    let end = wire.at + rdlen;
    match &record.data {
        Rdata::A(ip) => {
            assert_eq!(rtype, wire::TYPE_A);
            assert_eq!(wire.take(4), ip.octets());
        }
        Rdata::Srv {
            priority,
            weight,
            port,
            target,
        } => {
            assert_eq!(rtype, wire::TYPE_SRV);
            assert_eq!(
                [wire.word(), wire.word(), wire.word()],
                [*priority, *weight, *port]
            );
            assert_eq!(wire.name(), labels_of(target), "the service target");
        }
        Rdata::Txt(strings) => {
            assert_eq!(rtype, wire::TYPE_TXT);
            let mut text = Vec::new();
            while wire.at < end {
                let len = usize::from(wire.take(1).first().copied().unwrap_or(0));
                text.extend_from_slice(wire.take(len));
            }
            let expected: Vec<u8> = strings.iter().flat_map(|s| s.bytes()).collect();
            assert_eq!(text, expected, "the text strings");
        }
        Rdata::Mx {
            preference,
            exchange,
        } => {
            assert_eq!(rtype, wire::TYPE_MX);
            assert_eq!(wire.word(), *preference);
            assert_eq!(wire.name(), labels_of(exchange), "the exchanger");
        }
    }
    assert_eq!(wire.at, end, "the record's length does not match its data");
}

/// Writes the reply to a query over TCP and over UDP and reads each answer back.
fn check_reply(query: &wire::Query, question: &[u8], reply: &Reply) {
    let limit = query
        .edns_payload
        .map_or(512, |p| usize::from(p.min(wire::EDNS_PAYLOAD)));
    let opt = usize::from(query.edns_payload.is_some());
    let full = wire::respond(query, reply.rcode, &reply.answers, &reply.additional, false);
    for udp in [false, true] {
        let answer = wire::respond(query, reply.rcode, &reply.answers, &reply.additional, udp);
        let mut wire = Wire {
            bytes: &answer,
            at: 0,
        };
        assert_eq!(wire.word(), query.id, "the id");
        let flags = wire.word();
        assert!(flags & 0x8000 != 0, "the response bit");
        assert_eq!(
            flags & 0x0100 != 0,
            query.recursion_desired,
            "recursion desired is echoed"
        );
        assert_eq!(flags & 0xF, reply.rcode.code(), "the response code");
        let truncated = flags & 0x0200 != 0;
        assert_eq!(truncated, udp && full.len() > limit, "the truncated bit");
        let counts = [wire.word(), wire.word(), wire.word(), wire.word()];
        assert_eq!(counts[0], 1, "one question");
        assert_eq!(
            wire.take(question.len()),
            question,
            "the question as it was sent"
        );
        if truncated {
            assert_eq!(
                counts[1..],
                [0, 0, u16::try_from(opt).unwrap_or(0)],
                "a cut answer keeps no records"
            );
        } else {
            assert_eq!(
                usize::from(counts[1]),
                reply.answers.len(),
                "the answer count"
            );
            assert_eq!(
                usize::from(counts[3]),
                reply.additional.len() + opt,
                "the additional count"
            );
            for record in reply.answers.iter().chain(&reply.additional) {
                check_record(&mut wire, record);
            }
        }
        if opt == 1 {
            assert_eq!(wire.take(1), [0], "the OPT owner is the root");
            assert_eq!(wire.word(), 41, "the OPT type");
            assert_eq!(wire.word(), wire::EDNS_PAYLOAD, "our payload size");
            wire.take(6);
        }
        assert_eq!(wire.at, answer.len(), "bytes after the last record");
        if udp && !truncated {
            assert!(answer.len() <= limit, "a UDP answer over the limit");
        }
    }
}
