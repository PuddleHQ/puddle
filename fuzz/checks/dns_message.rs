// SPDX-License-Identifier: GPL-3.0-or-later
//! The checks of the `dns_message` cargo-fuzz target: the DNS message the stub reads from the
//! guest. Included by the fuzz target and by the stable corpus replay in `puddle-agent`'s tests
//! (a `fuzzing` feature on the shipped agent would fail the release-feature check). It takes arbitrary bytes, never panics, and panics only when an invariant the stub's
//! refusals and answers rest on does not hold.

use puddle_agent::capture::wire::{self, QueryName, Rcode, Rdata, Record, Reject};

/// Reads `bytes` as a DNS message from the guest and builds the answers the stub would send.
///
/// # Panics
///
/// When a name the parser called plain has a character outside letters, digits, `-`, `_` and `.`
/// or is longer than a DNS name, when an answer does not echo the message id, answers a query
/// without the response bit, or a UDP answer is bigger than the client allowed.
pub fn dns_message(bytes: &[u8]) {
    // The id and the high flag byte of a message.
    let head = |msg: &[u8]| -> Option<(u16, u8)> {
        let &[a, b, flags, ..] = msg else {
            return None;
        };
        Some((u16::from_be_bytes([a, b]), flags))
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
            let answer_head = head(&answer);
            assert_eq!(answer_head.map(|h| h.0), Some(id), "the id was not echoed");
            assert!(
                answer_head.is_some_and(|h| h.1 & 0x80 != 0),
                "the response bit is not set"
            );
            assert_eq!(
                head(bytes).map(|h| h.0),
                Some(id),
                "the id is not the message's"
            );
        }
        Ok(query) => {
            assert_eq!(
                head(bytes).map(|h| h.0),
                Some(query.id),
                "the id is not the message's"
            );
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
            // 512 bytes without an OPT record, else what the client advertised, at most the stub's own size.
            let limit = query.edns_payload.map_or(512, |p| {
                assert!(p >= 512, "an advertised size below 512 was not raised");
                usize::from(p.min(wire::EDNS_PAYLOAD))
            });
            for answers in [&[][..], &long[..], &short[..]] {
                for udp in [false, true] {
                    let answer = wire::respond(&query, Rcode::NoError, answers, &[], udp);
                    let answer_head = head(&answer);
                    assert_eq!(
                        answer_head.map(|h| h.0),
                        Some(query.id),
                        "the id was not echoed"
                    );
                    assert!(
                        answer_head.is_some_and(|h| h.1 & 0x80 != 0),
                        "the response bit is not set"
                    );
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
