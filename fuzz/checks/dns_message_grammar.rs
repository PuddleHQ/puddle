// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checks of the `dns_message_grammar` cargo-fuzz target: DNS messages built from the wire
//! grammar, so most inputs get past the header and the first label. Included, beside
//! `dns_message.rs` as `dns_message`, by the fuzz target and by the stable corpus replay in
//! `puddle-agent`'s tests.

use arbitrary::{Arbitrary, Result as ArbResult, Unstructured};
use std::net::Ipv4Addr;

use puddle_agent::capture::wire::{self, QueryName, Rcode, Rdata, Record, Reject};

use super::dns_message::dns_message;

/// A DNS message built from the wire grammar instead of from bytes: a header with the right and the
/// wrong counts, names made of labels (also over-long ones, compression pointers that point
/// forward, at themselves or at the question), records with an honest or a lying length, an `OPT`
/// record, a cut-off tail or trailing bytes. It carries what the stub must make of it.
#[derive(Debug)]
pub(crate) struct Message {
    bytes: Vec<u8>,
    expect: Expect,
    /// What the stub answers if the message is a query, to read back from the wire.
    reply: Reply,
}

/// The answer the stub is asked to write: a code and records of every kind, plain names only (the
/// caller of `respond` promises labels of 1 to 63 bytes and at most 253 bytes in all).
#[derive(Debug)]
struct Reply {
    rcode: Rcode,
    answers: Vec<Record>,
    additional: Vec<Record>,
}

#[derive(Debug)]
enum Expect {
    /// Too short to have an id, or a response: dropped.
    Drop,
    /// Answered with this code.
    Answer(Rcode),
    /// Read as this query; `edns` is `Unchecked` when the message has a shape the model does not follow.
    Query {
        name: QueryName,
        qtype: u16,
        qclass: u16,
        edns: Edns,
        question: Vec<u8>,
    },
    /// Cut off or counted wrongly in a way the model does not follow: only the byte-level checks apply.
    Unchecked,
}

/// The `OPT` record's advertised payload size the stub must find.
#[derive(Debug)]
enum Edns {
    Unchecked,
    Absent,
    Size(u16),
}

#[derive(Debug)]
enum Pointer {
    Question,
    Own,
    Forward(u8),
    Any(u16),
}

#[derive(Debug)]
enum WireName {
    Root,
    Labels(Vec<Vec<u8>>),
    Pointer(Pointer),
    LabelsThenPointer(Vec<Vec<u8>>, Pointer),
}

impl WireName {
    fn labels(&self) -> &[Vec<u8>] {
        match self {
            Self::Labels(labels) | Self::LabelsThenPointer(labels, _) => labels,
            Self::Root | Self::Pointer(_) => &[],
        }
    }

    fn is_plain_name(&self) -> bool {
        matches!(self, Self::Root | Self::Labels(_))
    }

    /// Whether a name of this shape is within the stub's limits: labels of at most 63 bytes and at
    /// most 255 bytes in all, root included.
    fn fits(&self) -> bool {
        let labels = self.labels();
        self.is_plain_name()
            && labels.iter().all(|l| l.len() <= 63)
            && labels.iter().map(|l| l.len() + 1).sum::<usize>() < 255
    }

    fn write(&self, out: &mut Vec<u8>) {
        let pointer = |out: &mut Vec<u8>, p: &Pointer| {
            let target = match p {
                Pointer::Question => 12,
                Pointer::Own => u16::try_from(out.len()).unwrap_or(0),
                Pointer::Forward(n) => u16::try_from(out.len()).unwrap_or(0) + u16::from(*n) + 1,
                Pointer::Any(n) => *n,
            };
            out.extend_from_slice(&(0xC000 | (target & 0x3FFF)).to_be_bytes());
        };
        match self {
            Self::Root => out.push(0),
            Self::Labels(labels) => {
                for label in labels {
                    out.push(u8::try_from(label.len()).unwrap_or(u8::MAX));
                    out.extend_from_slice(label);
                }
                out.push(0);
            }
            Self::Pointer(p) => pointer(out, p),
            Self::LabelsThenPointer(labels, p) => {
                for label in labels {
                    out.push(u8::try_from(label.len()).unwrap_or(u8::MAX));
                    out.extend_from_slice(label);
                }
                pointer(out, p);
            }
        }
    }

    /// The name as the stub reads it, when it is a plain one.
    fn expected(&self) -> QueryName {
        let mut name = String::new();
        for label in self.labels() {
            if !name.is_empty() {
                name.push('.');
            }
            for &b in label {
                if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') {
                    name.push(char::from(b.to_ascii_lowercase()));
                } else {
                    return QueryName::Odd;
                }
            }
        }
        QueryName::Plain(name)
    }
}

/// One of `common`, or sometimes any 16-bit value.
fn word(u: &mut Unstructured<'_>, common: &[u16]) -> ArbResult<u16> {
    if ratio(u, 1, 5)? {
        u.arbitrary()
    } else {
        Ok(*u.choose(common)?)
    }
}

fn ratio(u: &mut Unstructured<'_>, n: u32, d: u32) -> ArbResult<bool> {
    u.ratio(n, d)
}

fn pointer(u: &mut Unstructured<'_>) -> ArbResult<Pointer> {
    Ok(match u.int_in_range(0..=3)? {
        0 => Pointer::Question,
        1 => Pointer::Own,
        2 => Pointer::Forward(u.arbitrary()?),
        _ => Pointer::Any(u.arbitrary()?),
    })
}

/// Labels of 1 to 63 bytes (rarely 64 to 70), plain characters or any byte.
fn labels(u: &mut Unstructured<'_>, longest: u8) -> ArbResult<Vec<Vec<u8>>> {
    let mut labels = Vec::new();
    let count = if ratio(u, 1, 30)? { 200 } else { 5 };
    for _ in 0..u.int_in_range(1..=count)? {
        let len = match u.int_in_range(0..=11)? {
            0 => u.int_in_range(1..=longest)?,
            1 => *u.choose(&[63, 64, longest])?,
            _ => u.int_in_range(1..=12)?,
        };
        let plain = ratio(u, 3, 4)?;
        let mut label = Vec::new();
        for _ in 0..len {
            label.push(if plain {
                *u.choose(b"abcXYZ019-_az")?
            } else {
                u.arbitrary()?
            });
        }
        labels.push(label);
    }
    Ok(labels)
}

fn wire_name(u: &mut Unstructured<'_>, longest: u8) -> ArbResult<WireName> {
    Ok(match u.int_in_range(0..=9)? {
        0 => WireName::Root,
        1 => WireName::Pointer(pointer(u)?),
        2 => WireName::LabelsThenPointer(labels(u, longest)?, pointer(u)?),
        _ => WireName::Labels(labels(u, longest)?),
    })
}

/// A header count: the real one, or sometimes a wrong one.
fn count(u: &mut Unstructured<'_>, real: u16) -> ArbResult<u16> {
    Ok(if ratio(u, 1, 8)? {
        u.arbitrary()?
    } else {
        real
    })
}

/// A question as written: its name, its bytes, its type and its class.
type Question = (WireName, Vec<u8>, u16, u16);

/// What the grammar made of one message, for deciding what the stub must do with it.
struct Shape {
    /// The first question, as written, when the message was not cut or damaged after it.
    first: Option<Question>,
    /// Whether the scan of the additional section can be modelled: one question, honest records.
    scanned: bool,
    records: Vec<(u16, u16)>,
    additional: u16,
}

impl Shape {
    /// What the stub must do with `bytes`. The header is read from the bytes, not from what the
    /// grammar meant to write, because a cut inside the header followed by padding rewrites it.
    fn expect(self, bytes: &[u8]) -> Expect {
        let word = |at: usize| {
            bytes
                .get(at..at + 2)
                .and_then(|b| <[u8; 2]>::try_from(b).ok())
                .map_or(0, u16::from_be_bytes)
        };
        let flags = word(2);
        let (questions, answers, authority) = (word(4), word(6), word(8));
        if bytes.len() < 12 || flags & 0x8000 != 0 {
            return Expect::Drop;
        }
        if (flags >> 11) & 0xF != 0 {
            return Expect::Answer(Rcode::NotImp);
        }
        if questions != 1 || answers != 0 || authority != 0 {
            return Expect::Answer(Rcode::FormErr);
        }
        let Some((name, question, qtype, qclass)) = self.first else {
            return Expect::Unchecked;
        };
        if !name.fits() {
            return Expect::Answer(Rcode::FormErr);
        }
        let edns = if self.scanned && word(10) == self.additional {
            self.records
                .iter()
                .take(usize::from(self.additional.min(8)))
                .find(|(rtype, _)| *rtype == 41)
                .map_or(Edns::Absent, |(_, class)| Edns::Size((*class).max(512)))
        } else {
            Edns::Unchecked
        };
        Expect::Query {
            name: name.expected(),
            qtype,
            qclass,
            edns,
            question,
        }
    }
}

fn bytes_of(u: &mut Unstructured<'_>, most: usize) -> ArbResult<Vec<u8>> {
    (0..u.int_in_range(0..=most)?)
        .map(|_| u.arbitrary())
        .collect()
}

/// What `body` wrote: the first question, the (type, class) of each record after the questions, and
/// whether every record's length is honest.
type Body = (Option<Question>, Vec<(u16, u16)>, bool);

/// The questions, the answers and the records after them.
fn body(
    u: &mut Unstructured<'_>,
    out: &mut Vec<u8>,
    questions: u16,
    others: u16,
) -> ArbResult<Body> {
    let mut first = None;
    for _ in 0..questions {
        let name = wire_name(u, 70)?;
        let start = out.len();
        name.write(out);
        let qtype = word(u, &[1, 1, 12, 15, 16, 28, 33, 255])?;
        let qclass = word(u, &[1, 1, 1, 3, 255])?;
        out.extend_from_slice(&qtype.to_be_bytes());
        out.extend_from_slice(&qclass.to_be_bytes());
        if first.is_none() {
            first = Some((
                name,
                out.get(start..).unwrap_or_default().to_vec(),
                qtype,
                qclass,
            ));
        }
    }
    let mut records = Vec::new();
    let mut honest = true;
    for _ in 0..others {
        let owner = match u.int_in_range(0..=2)? {
            0 => WireName::Root,
            1 => WireName::Pointer(pointer(u)?),
            _ => WireName::Labels(labels(u, 63)?),
        };
        owner.write(out);
        let rtype = word(u, &[41, 41, 1, 16, 33])?;
        let class = word(u, &[0, 511, 512, 1232, 4096, 65535])?;
        let ttl: u32 = u.arbitrary()?;
        let rdata = bytes_of(u, 12)?;
        let rdlen = if ratio(u, 1, 10)? {
            honest = false;
            u.arbitrary()?
        } else {
            u16::try_from(rdata.len()).unwrap_or(0)
        };
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&class.to_be_bytes());
        out.extend_from_slice(&ttl.to_be_bytes());
        out.extend_from_slice(&rdlen.to_be_bytes());
        out.extend_from_slice(&rdata);
        records.push((rtype, class));
    }
    Ok((first, records, honest))
}

impl<'a> Arbitrary<'a> for Message {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        let id: u16 = u.arbitrary()?;
        let response = ratio(u, 1, 8)?;
        let opcode: u16 = if ratio(u, 1, 6)? {
            u.int_in_range(0..=15)?
        } else {
            0
        };
        let recursion_desired = u.arbitrary::<bool>()?;
        // The bits the stub ignores: AA, TC, RA, Z, AD, CD and the incoming response code.
        let ignored = u.arbitrary::<u16>()? & 0x06FF;
        let flags = (u16::from(response) << 15)
            | (opcode << 11)
            | (u16::from(recursion_desired) << 8)
            | ignored;

        let questions = if ratio(u, 1, 6)? {
            u.int_in_range(0..=2)?
        } else {
            1
        };
        let answers = if ratio(u, 1, 10)? {
            u.int_in_range(1..=2)?
        } else {
            0
        };
        let authority = if ratio(u, 1, 10)? {
            u.int_in_range(1..=2)?
        } else {
            0
        };
        let additional = u.int_in_range(0..=4)?;
        let header_counts = [
            count(u, questions)?,
            count(u, answers)?,
            count(u, authority)?,
            count(u, additional)?,
        ];

        let mut out = Vec::new();
        out.extend_from_slice(&id.to_be_bytes());
        out.extend_from_slice(&flags.to_be_bytes());
        for n in header_counts {
            out.extend_from_slice(&n.to_be_bytes());
        }
        let (first, records, honest) =
            body(u, &mut out, questions, answers + authority + additional)?;

        let mut intact = true;
        if ratio(u, 1, 8)? {
            let cut = u.int_in_range(0..=out.len())?;
            out.truncate(cut);
            intact = false;
        }
        if ratio(u, 1, 8)? {
            out.extend_from_slice(&bytes_of(u, 9)?);
        }
        let shape = Shape {
            first: first.filter(|_| intact),
            scanned: questions == 1 && answers == 0 && authority == 0 && honest,
            records,
            additional,
        };
        let expect = shape.expect(&out);
        Ok(Self {
            bytes: out,
            expect,
            reply: reply(u)?,
        })
    }
}

impl Message {
    /// What the grammar says the stub does with the message: `drop`, `answer` (an error code),
    /// `query` or `unchecked`.
    #[cfg(test)]
    pub(crate) fn shape(&self) -> &'static str {
        match self.expect {
            Expect::Drop => "drop",
            Expect::Answer(_) => "answer",
            Expect::Query { .. } => "query",
            Expect::Unchecked => "unchecked",
        }
    }

    /// The message as the stub reads it.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// Reads a grammar-built message and checks what the stub must make of it, then runs the
/// byte-level checks of [`dns_message`] on the same bytes.
///
/// # Panics
///
/// When the stub drops, refuses or reads a message differently from what its grammar says, when
/// the answer to a query does not carry the question as it was sent, or on anything
/// [`dns_message`] panics on.
pub(crate) fn dns_message_grammar(message: &Message) {
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

/// A plain name: up to five labels of letters, digits, `-` and `_`, at most 253 bytes in all.
fn plain_name(u: &mut Unstructured<'_>) -> ArbResult<String> {
    let mut name = String::new();
    for _ in 0..u.int_in_range(0..=5)? {
        let len = if ratio(u, 1, 4)? {
            u.int_in_range(1..=63)?
        } else {
            u.int_in_range(1..=10)?
        };
        let label: String = (0..len)
            .map(|_| u.choose(b"abcxyz019-_").map(|b| char::from(*b)))
            .collect::<ArbResult<_>>()?;
        if name.len() + label.len() + 1 > 253 {
            break;
        }
        if !name.is_empty() {
            name.push('.');
        }
        name.push_str(&label);
    }
    Ok(name)
}

fn record(u: &mut Unstructured<'_>) -> ArbResult<Record> {
    let owner = if ratio(u, 1, 2)? {
        None
    } else {
        Some(plain_name(u)?)
    };
    let data = match u.int_in_range(0..=3)? {
        0 => Rdata::A(Ipv4Addr::from(u.arbitrary::<u32>()?)),
        1 => Rdata::Srv {
            priority: u.arbitrary()?,
            weight: u.arbitrary()?,
            port: u.arbitrary()?,
            target: plain_name(u)?,
        },
        2 => {
            let mut strings = Vec::new();
            for _ in 0..u.int_in_range(0..=4)? {
                let len = if ratio(u, 1, 4)? {
                    u.int_in_range(0..=600)?
                } else {
                    u.int_in_range(0..=5)?
                };
                strings.push("t".repeat(len));
            }
            Rdata::Txt(strings)
        }
        _ => Rdata::Mx {
            preference: u.arbitrary()?,
            exchange: plain_name(u)?,
        },
    };
    Ok(Record {
        owner,
        ttl: u.arbitrary()?,
        data,
    })
}

fn reply(u: &mut Unstructured<'_>) -> ArbResult<Reply> {
    let rcode = *u.choose(&[
        Rcode::NoError,
        Rcode::NoError,
        Rcode::FormErr,
        Rcode::ServFail,
        Rcode::NxDomain,
        Rcode::NotImp,
        Rcode::Refused,
    ])?;
    let mut records = |most| -> ArbResult<Vec<Record>> {
        (0..u.int_in_range(0..=most)?).map(|_| record(u)).collect()
    };
    Ok(Reply {
        rcode,
        answers: records(12)?,
        additional: records(2)?,
    })
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
