// SPDX-License-Identifier: AGPL-3.0-or-later
//! Entry points for the `resolve_line` cargo-fuzz target: the lookup lines the host reads from
//! the guest agent (and the agent reads back). Each function takes arbitrary bytes, never panics,
//! and panics only when an invariant the lines rest on does not hold.

use arbitrary::{Arbitrary, Result as ArbResult, Unstructured};
use std::fmt::Write as _;

use futures_util::FutureExt;

mod ask;
pub use ask::{AskExchange, resolve_ask};

use crate::resolve::{
    MAX_ANSWER_LINE, MAX_QUERY_LINE, Record, RecordType, ResolveAnswer, ResolveError, ResolveQuery,
    StandInReason, read_query,
};

/// Reads `bytes` as a query line, as a stream of query lines and as an answer line.
///
/// # Panics
///
/// When a query the parser accepted does not write and read back as itself, when the stream
/// reader takes more than [`MAX_QUERY_LINE`] bytes for one query, or when an answer the parser
/// accepted does not write and read back as itself.
pub fn resolve_line(bytes: &[u8]) {
    if let Ok(query) = ResolveQuery::from_line(bytes)
        && let Ok(line) = query.to_line()
    {
        let again = ResolveQuery::from_line(&line);
        assert_eq!(
            again.ok().as_ref(),
            Some(&query),
            "an accepted query does not read back as itself"
        );
    }

    let mut reader = bytes;
    // A slice never waits, so the read finishes on its first poll.
    if let Some(Ok(streamed)) = read_query(&mut reader).now_or_never() {
        let taken = bytes.get(..bytes.len() - reader.len()).unwrap_or_default();
        assert!(
            taken.len() <= MAX_QUERY_LINE,
            "a query line over the limit was accepted"
        );
        assert_eq!(
            ResolveQuery::from_line(taken).ok(),
            Some(streamed),
            "the stream reader and the line parser disagree"
        );
    }

    if let Ok(answer) = ResolveAnswer::from_line(bytes)
        && let Ok(line) = answer.to_line()
    {
        let again = ResolveAnswer::from_line(&line);
        assert_eq!(
            again.ok().as_ref(),
            Some(&answer),
            "an accepted answer does not read back as itself"
        );
    }
}

/// A lookup or answer line built from a grammar: well-formed JSON in many spellings (key order,
/// white space, escapes, unknown fields), and a few broken ones. It knows what the host must make of
/// it, so [`resolve_line_grammar`] checks more than the byte-level target can.
#[derive(Debug)]
pub struct LineGrammar {
    bytes: Vec<u8>,
    expect: Expect,
}

#[derive(Debug)]
enum Expect {
    Query(ResolveQuery),
    Answer(ResolveAnswer),
    NotAQuery,
    NotAnAnswer,
}

impl LineGrammar {
    /// Decodes the grammar from fuzzer input, as the cargo-fuzz target does; `None` when the input
    /// is too short to decode.
    #[must_use]
    pub fn from_input(input: &[u8]) -> Option<Self> {
        Self::arbitrary(&mut Unstructured::new(input)).ok()
    }

    /// What the grammar says the host does with the line: `query`, `answer`, `not a query` or
    /// `not an answer`.
    #[must_use]
    pub fn shape(&self) -> &'static str {
        match self.expect {
            Expect::Query(_) => "query",
            Expect::Answer(_) => "answer",
            Expect::NotAQuery => "not a query",
            Expect::NotAnAnswer => "not an answer",
        }
    }

    /// The line as the bytes the parsers read.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A name of labels drawn from plain characters, punctuation JSON escapes, and non-ASCII text.
fn grammar_name(u: &mut Unstructured<'_>) -> ArbResult<String> {
    const ODD: [char; 10] = [
        'é',
        '日',
        '\u{1F986}',
        '"',
        '\\',
        '\n',
        '\0',
        '\u{7f}',
        '.',
        '/',
    ];
    let mut name = String::new();
    for index in 0..u.int_in_range(0..=6)? {
        if index > 0 {
            name.push('.');
        }
        let width = if u.ratio(1, 20)? { 70 } else { 12 };
        for _ in 0..u.int_in_range(0..=width)? {
            name.push(match u.int_in_range(0..=9)? {
                0..=4 => u.choose(b"abcdefghijklmnopqrstuvwxyz")?.to_owned().into(),
                5 => u.choose(b"0123456789")?.to_owned().into(),
                6 => '-',
                7 => '_',
                8 => *u.choose(&ODD)?,
                _ => u.arbitrary::<char>()?,
            });
        }
    }
    if u.ratio(1, 8)? {
        name.push('.');
    }
    Ok(name)
}

/// `text` as a JSON string: serde's spelling, or every non-alphanumeric character as `\u` escapes.
fn grammar_string(u: &mut Unstructured<'_>, text: &str) -> ArbResult<String> {
    if u.arbitrary::<bool>()? {
        return Ok(serde_json::Value::from(text).to_string());
    }
    let mut out = String::from("\"");
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out.push('"');
    Ok(out)
}

fn grammar_space(u: &mut Unstructured<'_>) -> ArbResult<&'static str> {
    Ok(*u.choose(&["", " ", "\t", "  ", "\r"])?)
}

/// Spells `members` (key, value) as a JSON object in a random order with random white space and,
/// sometimes, a field the parsers do not know, then ends the line.
fn grammar_object(
    u: &mut Unstructured<'_>,
    mut members: Vec<(&'static str, String)>,
) -> ArbResult<Vec<u8>> {
    if u.ratio(1, 3)? {
        members.reverse();
    }
    if u.ratio(1, 4)? {
        let extra = *u.choose(&["1", "null", "[]", "{\"a\":[1,{}]}", "\"x\"", "-1.5e3"])?;
        members.insert(
            u.int_in_range(0..=members.len())?,
            ("future", extra.to_owned()),
        );
    }
    let mut out = String::from("{");
    for (index, (key, value)) in members.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        let (a, b, c) = (grammar_space(u)?, grammar_space(u)?, grammar_space(u)?);
        let _ = write!(out, "{a}\"{key}\"{b}:{c}{value}");
    }
    out.push_str(grammar_space(u)?);
    out.push('}');
    out.push_str(u.choose(&["\n", "\n", "\r\n", ""])?);
    Ok(out.into_bytes())
}

fn grammar_ttl(u: &mut Unstructured<'_>) -> ArbResult<u32> {
    Ok(match u.int_in_range(0..=3)? {
        0 => 0,
        1 => u32::MAX,
        _ => u.arbitrary()?,
    })
}

fn grammar_record(u: &mut Unstructured<'_>) -> ArbResult<(Record, String)> {
    let target = grammar_name(u)?;
    Ok(match u.int_in_range(0..=2)? {
        0 => {
            let (priority, weight, port) = (u.arbitrary()?, u.arbitrary()?, u.arbitrary()?);
            let target_json = grammar_string(u, &target)?;
            (
                Record::Srv {
                    priority,
                    weight,
                    port,
                    target,
                },
                format!(
                    "{{\"type\":\"srv\",\"priority\":{priority},\"weight\":{weight},\"port\":{port},\"target\":{target_json}}}"
                ),
            )
        }
        1 => {
            let mut strings = Vec::new();
            for _ in 0..u.int_in_range(0..=4)? {
                let mut text = grammar_name(u)?;
                // Now and then a string big enough to push the line over its limit.
                if u.ratio(1, 8)? {
                    text = text.repeat(u.int_in_range(50..=600)?);
                }
                strings.push(text);
            }
            let json: Vec<String> = strings
                .iter()
                .map(|s| grammar_string(u, s))
                .collect::<ArbResult<_>>()?;
            (
                Record::Txt { strings },
                format!("{{\"type\":\"txt\",\"strings\":[{}]}}", json.join(",")),
            )
        }
        _ => {
            let preference = u.arbitrary()?;
            let target_json = grammar_string(u, &target)?;
            (
                Record::Mx {
                    preference,
                    exchange: target,
                },
                format!(
                    "{{\"type\":\"mx\",\"preference\":{preference},\"exchange\":{target_json}}}"
                ),
            )
        }
    })
}

impl<'a> Arbitrary<'a> for LineGrammar {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        if u.arbitrary::<bool>()? {
            Self::query(u)
        } else {
            Self::answer(u)
        }
    }
}

impl LineGrammar {
    fn query(u: &mut Unstructured<'_>) -> ArbResult<Self> {
        let name = grammar_name(u)?;
        let name_json = grammar_string(u, &name)?;
        let (rtype, rtype_json) = *u.choose(&[
            (Some(RecordType::A), "\"A\""),
            (Some(RecordType::Srv), "\"SRV\""),
            (Some(RecordType::Txt), "\"TXT\""),
            (Some(RecordType::Mx), "\"MX\""),
            (None, "\"AAAA\""),
            (None, "\"a\""),
            (None, "\"\""),
            (None, "1"),
            (None, "null"),
        ])?;
        let mut members = vec![("name", name_json.clone()), ("type", rtype_json.to_owned())];
        let mut broken = rtype.is_none();
        match u.int_in_range(0..=9)? {
            0 => {
                members.remove(0);
                broken = true;
            }
            1 => {
                members.remove(1);
                broken = true;
            }
            2 => {
                members.push(("name", name_json));
                broken = true;
            }
            3 => {
                if let Some(member) = members.first_mut() {
                    "7".clone_into(&mut member.1);
                }
                broken = true;
            }
            _ => {}
        }
        let bytes = grammar_object(u, members)?;
        let expect = match rtype {
            Some(rtype) if !broken => Expect::Query(ResolveQuery::new(name, rtype)),
            _ => Expect::NotAQuery,
        };
        Ok(Self { bytes, expect })
    }

    fn answer(u: &mut Unstructured<'_>) -> ArbResult<Self> {
        let ttl = grammar_ttl(u)?;
        let ttl_member = ("ttl", ttl.to_string());
        let (members, expect) = match u.int_in_range(0..=9)? {
            0 => {
                let (why, tag) = *u.choose(&[
                    (StandInReason::Resolves, "resolves"),
                    (StandInReason::ViaUpstream, "via_upstream"),
                    (StandInReason::NotAllowed, "not_allowed"),
                    (StandInReason::Blocked, "blocked"),
                ])?;
                (
                    vec![
                        ("result", "\"stand_in\"".to_owned()),
                        ("why", format!("\"{tag}\"")),
                        ttl_member,
                    ],
                    Expect::Answer(ResolveAnswer::StandIn { why, ttl }),
                )
            }
            1 => (
                vec![("result", "\"no_such_name\"".to_owned()), ttl_member],
                Expect::Answer(ResolveAnswer::NoSuchName { ttl }),
            ),
            2 => (
                vec![("result", "\"no_data\"".to_owned()), ttl_member],
                Expect::Answer(ResolveAnswer::NoData { ttl }),
            ),
            3 => (
                vec![("result", "\"unavailable\"".to_owned())],
                Expect::Answer(ResolveAnswer::Unavailable),
            ),
            4 => (
                vec![("result", "\"from_the_future\"".to_owned()), ttl_member],
                Expect::Answer(ResolveAnswer::Unknown),
            ),
            5 => {
                // A ttl no `u32` holds.
                let bad = *u.choose(&["-1", "4294967296", "1.5", "\"60\"", "null"])?;
                (
                    vec![
                        ("result", "\"no_data\"".to_owned()),
                        ("ttl", bad.to_owned()),
                    ],
                    Expect::NotAnAnswer,
                )
            }
            _ => {
                let mut records = Vec::new();
                let mut json = Vec::new();
                for _ in 0..u.int_in_range(0..=20)? {
                    let (record, text) = grammar_record(u)?;
                    records.push(record);
                    json.push(text);
                }
                (
                    vec![
                        ("result", "\"records\"".to_owned()),
                        ("records", format!("[{}]", json.join(","))),
                        ttl_member,
                    ],
                    Expect::Answer(ResolveAnswer::Records { records, ttl }),
                )
            }
        };
        Ok(Self {
            bytes: grammar_object(u, members)?,
            expect,
        })
    }
}

/// Reads a grammar-built line and checks what the host must make of it, then runs the byte-level
/// checks of [`resolve_line`] on the same bytes.
///
/// # Panics
///
/// When a line the grammar made well-formed is refused or read as something else, when a line it
/// made wrong is accepted, or on anything [`resolve_line`] panics on.
pub fn resolve_line_grammar(line: &LineGrammar) {
    let bytes = line.bytes();
    match &line.expect {
        Expect::Query(query) => {
            assert_eq!(
                ResolveQuery::from_line(bytes).as_ref().ok(),
                Some(query),
                "a well-formed query was not read as itself"
            );
            assert_written_line(query.to_line(), query, MAX_QUERY_LINE);
            if bytes.len() <= MAX_QUERY_LINE && !bytes.is_empty() && bytes.ends_with(b"\n") {
                let mut reader = bytes;
                let streamed = read_query(&mut reader).now_or_never();
                assert!(
                    matches!(&streamed, Some(Ok(q)) if q == query),
                    "the stream reader did not take a well-formed query within the limit"
                );
            }
        }
        Expect::Answer(answer) => {
            assert_eq!(
                ResolveAnswer::from_line(bytes).as_ref().ok(),
                Some(answer),
                "a well-formed answer was not read as itself"
            );
            assert_written_line(answer.to_line(), answer, MAX_ANSWER_LINE);
        }
        Expect::NotAQuery => assert!(
            ResolveQuery::from_line(bytes).is_err(),
            "a broken query was accepted"
        ),
        Expect::NotAnAnswer => assert!(
            ResolveAnswer::from_line(bytes).is_err(),
            "a broken answer was accepted"
        ),
    }
    resolve_line(bytes);
}

/// A value written as a line is refused as too long exactly when its JSON and newline pass `limit`,
/// and otherwise reads back as itself.
fn assert_written_line<T>(written: Result<Vec<u8>, ResolveError>, value: &T, limit: usize)
where
    T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_vec(value).map_or(0, |json| json.len());
    match written {
        Ok(line) => {
            assert!(line.len() <= limit, "a line over the limit was written");
            assert_eq!(line.len(), json + 1, "the line is the JSON and a newline");
            let back = serde_json::from_slice::<T>(&line);
            assert_eq!(
                back.as_ref().ok(),
                Some(value),
                "a written line does not read back"
            );
        }
        Err(err) => assert!(
            matches!(err, ResolveError::TooLong) && json + 1 > limit,
            "a value within the limit was not written: {err}"
        ),
    }
}
