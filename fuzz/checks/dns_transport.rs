// SPDX-License-Identifier: AGPL-3.0-or-later
//! The checks of the `dns_udp_datagram` and `dns_tcp_stream` cargo-fuzz targets: how the stub reads
//! its two transports. UDP goes through a real socket on the loopback address (the receive buffer is the
//! length check); TCP goes through an in-memory stream, because a server that closes with unread bytes
//! resets the connection and a reset loses answers the test wants to read. Included, beside
//! `dns_message.rs` and `dns_message_grammar.rs`, by the fuzz targets and by the stable corpus replay in
//! `puddle-agent`'s tests.
//!
//! The expected answer to a message is what the stub gives the same message called directly
//! (`Stub::answer`); what is checked here is which bytes of the transport reach it, in which order, and
//! how the answer leaves.

#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "a fuzz check reports a broken invariant, and a rig that cannot be set up, by panicking"
)]

use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

use arbitrary::{Arbitrary, Result as ArbResult, Unstructured};
use puddle_agent::capture::dns::{Ask, DnsServer, Limits, Stub, tcp_connection};
use puddle_agent::capture::table::StandIns;
use puddle_agent::capture::wire::MAX_QUERY;
use puddle_agent_proto::resolve::{
    Record, RecordType, ResolveAnswer, ResolveError, ResolveQuery, StandInReason,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UdpSocket;

use super::dns_message_grammar::Message;

/// A host that answers from the name alone, so asking twice gives the same answer. `big.` names have
/// a text answer of about 3 KB: it fits a TCP answer and an advertised UDP size of 4096, and not the
/// 512 bytes a query without `OPT` allows.
struct Host;

impl Ask for Host {
    fn ask(
        &self,
        query: ResolveQuery,
    ) -> impl Future<Output = Result<ResolveAnswer, ResolveError>> + Send {
        let ttl = 30;
        let sum = query.name.bytes().fold(0usize, |sum, b| {
            sum.wrapping_mul(31).wrapping_add(usize::from(b))
        });
        let answer = if query.name.starts_with("big.") && query.rtype == RecordType::Txt {
            ResolveAnswer::Records {
                records: (0..12)
                    .map(|_| Record::Txt {
                        strings: vec!["t".repeat(250)],
                    })
                    .collect(),
                ttl,
            }
        } else {
            match sum % 5 {
                0 => ResolveAnswer::StandIn {
                    why: StandInReason::Resolves,
                    ttl,
                },
                1 => ResolveAnswer::NoSuchName { ttl },
                2 => ResolveAnswer::NoData { ttl },
                3 => ResolveAnswer::Unavailable,
                _ => ResolveAnswer::Records {
                    records: vec![Record::Mx {
                        preference: 10,
                        exchange: "mail.example.net".into(),
                    }],
                    ttl,
                },
            }
        };
        std::future::ready(Ok(answer))
    }
}

/// The runtime, the stub and its UDP socket, one set per thread.
struct Rig {
    runtime: tokio::runtime::Runtime,
    stub: Stub<Host>,
    udp: SocketAddr,
    _server: DnsServer,
}

/// A stub with an empty cache and no stand-in addresses handed out.
fn fresh_stub() -> Stub<Host> {
    Stub::new(Host, StandIns::in_memory(), Limits::default())
}

impl Rig {
    fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        let stub = fresh_stub();
        let server = runtime
            .block_on(DnsServer::bind(stub.clone(), ([127, 0, 0, 1], 0).into()))
            .expect("a loopback port");
        Self {
            udp: server.udp_addr(),
            runtime,
            stub,
            _server: server,
        }
    }
}

thread_local! {
    static RIG: Rig = Rig::new();
}

/// What a client puts in a message: a grammar-built DNS message, a query laid out so its `OPT` record
/// ends at a chosen byte (around the limits that matter: 512, 1232 and `MAX_QUERY`), or raw bytes.
#[derive(Debug)]
pub(crate) enum Probe {
    Grammar(Message),
    Bounded(Vec<u8>),
    Raw(Vec<u8>),
}

impl Probe {
    /// The message bytes.
    pub(crate) fn bytes(&self) -> &[u8] {
        match self {
            Self::Grammar(message) => message.bytes(),
            Self::Bounded(bytes) | Self::Raw(bytes) => bytes,
        }
    }

    /// Which of the three it is, for the corpus test.
    #[cfg(test)]
    pub(crate) fn shape(&self) -> &'static str {
        match self {
            Self::Grammar(_) => "grammar",
            Self::Bounded(_) => "bounded",
            Self::Raw(_) => "raw",
        }
    }
}

/// A query for `name` and `qtype` whose last record is an `OPT` advertising `size`, ending at byte
/// `end` of the message when that fits (a record of another type fills the space before it).
fn bounded(id: u16, name: &str, qtype: u16, size: u16, end: usize) -> Vec<u8> {
    let mut question = Vec::new();
    for label in name.split('.') {
        question.push(u8::try_from(label.len()).unwrap_or(0));
        question.extend_from_slice(label.as_bytes());
    }
    question.push(0);
    question.extend_from_slice(&qtype.to_be_bytes());
    question.extend_from_slice(&1u16.to_be_bytes());

    let opt = [&[0u8, 0, 41][..], &size.to_be_bytes(), &[0, 0, 0, 0, 0, 0]].concat();
    let plain = 12 + question.len() + opt.len();
    let filler = end.checked_sub(plain + 11);
    let mut out = Vec::new();
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&0x0100u16.to_be_bytes());
    out.extend_from_slice(&[0, 1, 0, 0, 0, 0]);
    out.extend_from_slice(&(if filler.is_some() { 2u16 } else { 1 }).to_be_bytes());
    out.extend_from_slice(&question);
    if let Some(pad) = filler {
        out.extend_from_slice(&[0, 0, 99, 0, 1, 0, 0, 0, 0]);
        out.extend_from_slice(&u16::try_from(pad).unwrap_or(u16::MAX).to_be_bytes());
        out.resize(out.len() + pad, 7);
    }
    out.extend_from_slice(&opt);
    out
}

impl<'a> Arbitrary<'a> for Probe {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        Ok(match u.int_in_range(0..=9)? {
            0..=4 => Self::Grammar(Message::arbitrary(u)?),
            5..=8 => {
                const ENDS: [usize; 14] = [
                    0, 511, 512, 513, 1231, 1232, 1233, 4094, 4095, 4096, 4096, 4097, 4098, 6000,
                ];
                let name = *u.choose(&["big.example.com", "small.example.com", "a.b"])?;
                let qtype = *u.choose(&[16u16, 16, 1, 15, 33])?;
                let size = *u.choose(&[512u16, 1232, 4096, 65535])?;
                Self::Bounded(bounded(
                    u.arbitrary()?,
                    name,
                    qtype,
                    size,
                    *u.choose(&ENDS)?,
                ))
            }
            _ => {
                let mut bytes = u.arbitrary::<Vec<u8>>()?;
                bytes.truncate(300);
                Self::Raw(bytes)
            }
        })
    }
}

/// The message the stub answers to prove a datagram was not mixed up with the one before it.
fn sentinel() -> Vec<u8> {
    bounded(0xBEEF, "small.example.com", 1, 1232, 0)
}

/// A query the stub answers, padded with zero bytes after the question to `len` bytes.
pub(crate) fn padded_query(len: usize) -> Vec<u8> {
    let mut message = sentinel();
    message.resize(len, 0);
    message
}

/// Sends one datagram to the stub's UDP socket and checks what comes back.
///
/// # Panics
///
/// When the answer (or the lack of one) differs from what the stub says to the
/// datagram called directly (a datagram over `MAX_QUERY` bytes is dropped, as on every OS), or
/// when no answer comes in two seconds.
pub(crate) fn dns_udp_datagram(probe: &Probe) {
    dns_udp_bytes(probe.bytes());
}

/// [`dns_udp_datagram`] for one datagram given as bytes.
///
/// # Panics
///
/// As [`dns_udp_datagram`].
pub(crate) fn dns_udp_bytes(sent: &[u8]) {
    RIG.with(|rig| {
        rig.runtime.block_on(async {
            let mut want: Vec<Vec<u8>> = if sent.len() > MAX_QUERY {
                Vec::new()
            } else {
                rig.stub.answer(sent, true).await.into_iter().collect()
            };
            let sentinel = sentinel();
            want.extend(rig.stub.answer(&sentinel, true).await);

            let socket = UdpSocket::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("a client port");
            socket
                .send_to(sent, rig.udp)
                .await
                .expect("send the datagram");
            socket
                .send_to(&sentinel, rig.udp)
                .await
                .expect("send the sentinel");

            let mut got = Vec::new();
            let mut buf = vec![0u8; 64 * 1024];
            while got.len() < want.len() {
                let read =
                    tokio::time::timeout(Duration::from_secs(2), socket.recv(&mut buf)).await;
                let Ok(Ok(n)) = read else {
                    panic!("{} of {} answers came: {got:?}", got.len(), want.len());
                };
                got.push(buf.get(..n).unwrap_or_default().to_vec());
            }
            // One more only if the stub answered what it should have dropped.
            if let Ok(Ok(n)) =
                tokio::time::timeout(Duration::from_millis(1), socket.recv(&mut buf)).await
            {
                got.push(buf.get(..n).unwrap_or_default().to_vec());
            }
            got.sort();
            want.sort();
            assert_eq!(
                got, want,
                "the datagrams were not answered as the stub answers them"
            );
        });
    });
}

/// How a frame states its length.
#[derive(Debug, Clone, Copy)]
enum Prefix {
    Honest,
    Over(u16),
    Under(u16),
    Zero,
    /// The stub's limit and one past it, whatever follows.
    Limit,
    PastLimit,
}

#[derive(Debug)]
struct Frame {
    probe: Probe,
    prefix: Prefix,
}

/// A TCP stream of length-prefixed messages: honest and lying prefixes, the limit and one past it, a
/// stream cut anywhere.
#[derive(Debug)]
pub(crate) struct Stream {
    frames: Vec<Frame>,
    cut: Option<usize>,
}

impl<'a> Arbitrary<'a> for Stream {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        let mut frames = Vec::new();
        for _ in 0..u.int_in_range(0..=5)? {
            let prefix = match u.int_in_range(0..=11)? {
                0..=6 => Prefix::Honest,
                7 => Prefix::Over(u.int_in_range(1..=3)?),
                8 => Prefix::Under(u.int_in_range(1..=3)?),
                9 => Prefix::Zero,
                10 => Prefix::Limit,
                _ => Prefix::PastLimit,
            };
            frames.push(Frame {
                probe: Probe::arbitrary(u)?,
                prefix,
            });
        }
        let cut = if u.ratio(1, 6)? {
            Some(u.int_in_range(0..=usize::from(u16::MAX))?)
        } else {
            None
        };
        Ok(Self { frames, cut })
    }
}

impl Stream {
    /// How each frame states its length, and whether the stream is cut, for the corpus test.
    #[cfg(test)]
    pub(crate) fn kinds(&self) -> Vec<&'static str> {
        let mut kinds: Vec<&'static str> = self
            .frames
            .iter()
            .map(|frame| match frame.prefix {
                Prefix::Honest => "honest",
                Prefix::Over(_) => "over",
                Prefix::Under(_) => "under",
                Prefix::Zero => "zero",
                Prefix::Limit => "limit",
                Prefix::PastLimit => "past the limit",
            })
            .collect();
        if self.cut.is_some() {
            kinds.push("cut");
        }
        kinds
    }

    fn bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for Frame { probe, prefix } in &self.frames {
            let body = probe.bytes();
            let honest = u16::try_from(body.len()).unwrap_or(u16::MAX);
            let claimed = match prefix {
                Prefix::Honest => honest,
                Prefix::Over(n) => honest.saturating_add(*n),
                Prefix::Under(n) => honest.saturating_sub(*n),
                Prefix::Zero => 0,
                Prefix::Limit => u16::try_from(MAX_QUERY).unwrap_or(u16::MAX),
                Prefix::PastLimit => u16::try_from(MAX_QUERY + 1).unwrap_or(u16::MAX),
            };
            out.extend_from_slice(&claimed.to_be_bytes());
            out.extend_from_slice(body);
        }
        if let Some(cut) = self.cut {
            out.truncate(cut);
        }
        out
    }
}

/// Feeds a TCP byte stream to the stub's connection handler and checks the answers.
///
/// # Panics
///
/// When the bytes that come back differ from this reading of the framing (a two-byte length that is
/// not zero and not over `MAX_QUERY`, that many bytes of message, an answer framed the same way,
/// and an end at the first prefix that fails, the first cut-off message or the first message the
/// stub drops), or when the connection does not end.
pub(crate) fn dns_tcp_stream(stream: &Stream) {
    let input = stream.bytes();
    RIG.with(|rig| {
        rig.runtime.block_on(async {
            // The stub keeps a cache, and an answer can depend on it (a type the stub does not
            // forward gets `NXDOMAIN` once an `A` query for the name was answered so). Two stubs
            // that start empty, one for the expected answers and one for the connection, so the
            // expected pass does not warm the cache the connection then reads, and an earlier
            // input leaves nothing behind.
            let oracle = fresh_stub();
            let stub = fresh_stub();
            let mut want = Vec::new();
            let mut rest = input.as_slice();
            while let Some((len, tail)) = rest.split_first_chunk::<2>() {
                let len = usize::from(u16::from_be_bytes(*len));
                if len == 0 || len > MAX_QUERY {
                    break;
                }
                let Some((message, tail)) = tail.split_at_checked(len) else {
                    break;
                };
                let Some(answer) = oracle.answer(message, false).await else {
                    break;
                };
                want.extend_from_slice(
                    &u16::try_from(answer.len())
                        .unwrap_or(u16::MAX)
                        .to_be_bytes(),
                );
                want.extend_from_slice(&answer);
                rest = tail;
            }

            let (client, server) = tokio::io::duplex(64 * 1024);
            let (mut read, mut write) = tokio::io::split(client);
            let handler = tcp_connection(server, stub, Duration::from_secs(5));
            let writer = async {
                // The handler may have closed already (a bad prefix), so a failed write is fine.
                let _ = write.write_all(&input).await;
                let _ = write.shutdown().await;
            };
            let reader = async {
                let mut got = Vec::new();
                let _ = read.read_to_end(&mut got).await;
                got
            };
            let all = async { tokio::join!(handler, writer, reader).2 };
            let got = tokio::time::timeout(Duration::from_secs(10), all)
                .await
                .expect("the connection did not end");
            assert_eq!(got, want, "the TCP answers");
        });
    });
}
