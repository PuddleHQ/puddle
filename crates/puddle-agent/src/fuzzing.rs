// SPDX-License-Identifier: GPL-3.0-or-later
//! Entry point for the `dns_message` cargo-fuzz target: the DNS message the stub reads from the
//! guest. It takes arbitrary bytes, never panics, and panics only when an invariant the stub's
//! refusals and answers rest on does not hold.

use crate::capture::wire::{self, QueryName, Rcode, Rdata, Record, Reject};

/// Reads `bytes` as a DNS message from the guest and builds the answers the stub would send.
///
/// # Panics
///
/// When a name the parser called plain has a character outside letters, digits, `-`, `_` and `.`
/// or is longer than a DNS name, when an answer does not echo the message id, answers a query
/// without the response bit, or a UDP answer is bigger than the client allowed.
#[expect(
    clippy::panic,
    reason = "a fuzz entry point reports a broken invariant by panicking"
)]
pub fn dns_message(bytes: &[u8]) {
    // The id and the high flag byte of a message.
    let head = |msg: &[u8]| -> (u16, u8) {
        let &[a, b, flags, ..] = msg else {
            panic!("a message shorter than its header was answered");
        };
        (u16::from_be_bytes([a, b]), flags)
    };
    match wire::parse_query(bytes) {
        Err(Reject::Drop) => {}
        Err(Reject::Answer {
            id,
            rcode,
            recursion_desired,
        }) => {
            let answer = wire::error_response(id, rcode, recursion_desired);
            assert_eq!(answer.len(), 12, "an error answer is the header alone");
            let (answer_id, flags) = head(&answer);
            assert_eq!(answer_id, id, "the id was not echoed");
            assert_ne!(flags & 0x80, 0, "the response bit is not set");
            assert_eq!(head(bytes).0, id, "the id is not the message's");
        }
        Ok(query) => {
            assert_eq!(head(bytes).0, query.id, "the id is not the message's");
            if let QueryName::Plain(name) = &query.name {
                assert!(
                    name.len() <= 255
                        && name.bytes().all(|b| b.is_ascii_lowercase()
                            || b.is_ascii_digit()
                            || matches!(b, b'-' | b'_' | b'.')),
                    "a plain name has a character it must not: {name:?}"
                );
            }
            // The long list is too big for a UDP answer, which must then be cut to the question.
            let record = Record {
                owner: None,
                ttl: 1,
                data: Rdata::Txt(vec!["x".repeat(255)]),
            };
            let long = vec![record.clone(); 40];
            let short = [record];
            let limit = query
                .edns_payload
                .map_or(512, |p| usize::from(p.min(wire::EDNS_PAYLOAD)));
            for answers in [&[][..], &long[..], &short[..]] {
                for udp in [false, true] {
                    let answer = wire::respond(&query, Rcode::NoError, answers, &[], udp);
                    let (id, flags) = head(&answer);
                    assert_eq!(id, query.id, "the id was not echoed");
                    assert_ne!(flags & 0x80, 0, "the response bit is not set");
                    if udp {
                        assert!(
                            answer.len() <= limit,
                            "a UDP answer of {} bytes passes the {limit} the client allowed",
                            answer.len()
                        );
                    }
                }
            }
        }
    }
}
