// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `resolve_ask` cargo-fuzz target: the agent's side of one lookup, [`crate::resolve::ask`], run
//! against a scripted host on an in-memory stream. The script is built from a grammar (an answer
//! padded to just under, at and just over the line limit, cut or empty replies, junk, a host that
//! goes quiet, a reply written in small pieces) and the outcome is compared with what the stream
//! rules say, worked out from the reply bytes alone.

use std::cell::OnceCell;
use std::time::Duration;

use arbitrary::{Arbitrary, Result as ArbResult, Unstructured};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use super::grammar_name;
use crate::resolve::{
    MAX_ANSWER_LINE, PREAMBLE, Record, RecordType, ResolveAnswer, ResolveError, ResolveQuery,
    StandInReason, ask,
};

/// The patience of a lookup against a host that goes quiet mid-line.
const SHORT: Duration = Duration::from_millis(10);
/// The patience when an answer is due: long enough that only a hang trips it.
const LONG: Duration = Duration::from_secs(5);

/// One lookup against a scripted host.
#[derive(Debug)]
pub struct AskExchange {
    query: ResolveQuery,
    reply: Vec<u8>,
    /// The answer the reply was built from, when it was built from one and not damaged.
    intended: Option<ResolveAnswer>,
    shape: &'static str,
    /// The host keeps the stream open after writing the reply.
    quiet: bool,
    /// Bytes the stream holds in each direction before a write waits.
    capacity: usize,
    /// How the reply is cut into writes.
    pieces: Vec<usize>,
}

impl AskExchange {
    /// Decodes the exchange from fuzzer input, as the cargo-fuzz target does; `None` when the
    /// input is too short to decode.
    #[must_use]
    pub fn from_input(input: &[u8]) -> Option<Self> {
        Self::arbitrary(&mut Unstructured::new(input)).ok()
    }

    /// What the grammar built: `answer`, `junk` or `cut`.
    #[must_use]
    pub fn shape(&self) -> &'static str {
        self.shape
    }
}

fn answer(u: &mut Unstructured<'_>) -> ArbResult<ResolveAnswer> {
    let ttl = u.arbitrary()?;
    Ok(match u.int_in_range(0..=5)? {
        0 => ResolveAnswer::StandIn {
            why: *u.choose(&[
                StandInReason::Resolves,
                StandInReason::ViaUpstream,
                StandInReason::NotAllowed,
                StandInReason::Blocked,
            ])?,
            ttl,
        },
        1 => ResolveAnswer::NoSuchName { ttl },
        2 => ResolveAnswer::NoData { ttl },
        3 => ResolveAnswer::Unavailable,
        4 => ResolveAnswer::Records {
            records: vec![Record::Mx {
                preference: u.arbitrary()?,
                exchange: grammar_name(u)?,
            }],
            ttl,
        },
        _ => {
            // Text that can fill a line, so the padding below has little left to add.
            let text = "t".repeat(u.int_in_range(0..=MAX_ANSWER_LINE)?);
            ResolveAnswer::Records {
                records: vec![Record::Txt {
                    strings: vec![text],
                }],
                ttl,
            }
        }
    })
}

impl<'a> Arbitrary<'a> for AskExchange {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        let rtype = *u.choose(&[
            RecordType::A,
            RecordType::Srv,
            RecordType::Txt,
            RecordType::Mx,
        ])?;
        let name = if u.ratio(1, 10)? {
            // Around the query line limit (512 bytes with the JSON around the name).
            "a".repeat(u.int_in_range(470..=500)?)
        } else {
            grammar_name(u)?
        };
        let query = ResolveQuery::new(name, rtype);

        let mut intended = None;
        let which = u.int_in_range(0..=9)?;
        let shape = match which {
            0..=5 => "answer",
            8 => "junk",
            _ => "cut",
        };
        let reply = match which {
            0..=5 => {
                let value = answer(u)?;
                let mut line = serde_json::to_vec(&value).unwrap_or_default();
                // White space before the newline keeps it an answer and moves the line length
                // to either side of the limit.
                let target = *u.choose(&[
                    0,
                    MAX_ANSWER_LINE - 1,
                    MAX_ANSWER_LINE,
                    MAX_ANSWER_LINE + 1,
                    MAX_ANSWER_LINE + 2,
                ])?;
                while line.len() + 1 < target {
                    line.push(b' ');
                }
                line.push(b'\n');
                if u.ratio(1, 6)? {
                    line.extend_from_slice(b"{\"result\":\"unavailable\"}\n");
                }
                intended = Some(value);
                line
            }
            6 => Vec::new(),
            7 => {
                // A valid answer with its newline cut off.
                let mut line = serde_json::to_vec(&answer(u)?).unwrap_or_default();
                if u.ratio(1, 3)? {
                    line.truncate(u.int_in_range(0..=line.len())?);
                }
                line
            }
            8 => {
                let mut junk = u.arbitrary::<Vec<u8>>()?;
                junk.retain(|b| *b != b'\n');
                junk.push(b'\n');
                junk
            }
            _ => {
                // No newline in sight, around the limit.
                vec![b'x'; u.int_in_range(MAX_ANSWER_LINE - 2..=MAX_ANSWER_LINE + 2)?]
            }
        };
        let mut pieces = Vec::new();
        if u.ratio(1, 3)? {
            for _ in 0..u.int_in_range(0..=6)? {
                pieces.push(u.int_in_range(1..=MAX_ANSWER_LINE)?);
            }
        }
        Ok(Self {
            query,
            reply,
            shape,
            intended,
            quiet: u.ratio(1, 32)?,
            capacity: *u.choose(&[16, 64, 1024, 8 * 1024, 64 * 1024])?,
            pieces,
        })
    }
}

/// What the stream rules make of a reply: the first line, if it ends within the limit, is the
/// answer; the stream ending inside a line, or a line past the limit, is an error.
#[derive(Debug, PartialEq)]
enum Outcome {
    Line(Vec<u8>),
    TooLong,
    Empty,
    Cut,
    Quiet,
}

fn outcome(reply: &[u8], quiet: bool) -> Outcome {
    match reply.iter().position(|b| *b == b'\n') {
        Some(end) if end < MAX_ANSWER_LINE => {
            Outcome::Line(reply.get(..=end).unwrap_or_default().to_vec())
        }
        Some(_) => Outcome::TooLong,
        None if reply.len() > MAX_ANSWER_LINE => Outcome::TooLong,
        None if quiet => Outcome::Quiet,
        None if reply.is_empty() => Outcome::Empty,
        None => Outcome::Cut,
    }
}

thread_local! {
    static RUNTIME: OnceCell<Option<tokio::runtime::Runtime>> = const { OnceCell::new() };
}

/// The lookup itself, run on this thread's runtime against the host script; `request` is what the host must
/// receive, `None` when the query is over the limit and nothing may reach the stream.
fn lookup(
    exchange: &AskExchange,
    request: Option<&[u8]>,
    limit: Duration,
) -> Option<Result<ResolveAnswer, ResolveError>> {
    let (client, host) = tokio::io::duplex(exchange.capacity);
    let host_script = async {
        let Some(request) = request else {
            return;
        };
        let mut host = host;
        let mut got = vec![0u8; request.len()];
        assert!(
            host.read_exact(&mut got).await.is_ok(),
            "the agent did not send the whole request"
        );
        assert_eq!(&got, request, "the request on the stream");
        let mut rest = exchange.reply.as_slice();
        let mut pieces = exchange.pieces.iter();
        while !rest.is_empty() {
            let take = pieces.next().copied().unwrap_or(rest.len()).min(rest.len());
            let (now, later) = rest.split_at(take);
            if host.write_all(now).await.is_err() {
                return;
            }
            rest = later;
            tokio::task::yield_now().await;
        }
        if exchange.quiet {
            std::future::pending::<()>().await;
        }
    };
    RUNTIME.with(|cell| {
        let runtime = cell.get_or_init(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .ok()
        });
        assert!(runtime.is_some(), "no current-thread runtime");
        let runtime = runtime.as_ref()?;
        Some(runtime.block_on(async {
            let lookup = ask(client, &exchange.query, limit);
            tokio::pin!(lookup);
            let host_script = host_script;
            tokio::pin!(host_script);
            let mut host_done = false;
            loop {
                tokio::select! {
                    biased;
                    got = &mut lookup => break got,
                    () = &mut host_script, if !host_done => host_done = true,
                }
            }
        }))
    })
}

/// Runs the lookup of `exchange` against its scripted host and checks the outcome.
///
/// # Panics
///
/// When the host does not receive the preamble and the query line exactly, when a query over the
/// limit reaches the stream, when an answer is read differently from the first line of the reply
/// (or from the answer the reply was built from), when a line past the limit is accepted or a
/// reply cut inside a line is not an error, or when a host that goes quiet is not given up on.
pub fn resolve_ask(exchange: &AskExchange) {
    let expected_request = exchange.query.to_line().ok().map(|line| {
        let mut request = PREAMBLE.to_vec();
        request.extend_from_slice(&line);
        request
    });
    let wanted = outcome(&exchange.reply, exchange.quiet);
    let limit = if wanted == Outcome::Quiet {
        SHORT
    } else {
        LONG
    };
    let Some(result) = lookup(exchange, expected_request.as_deref(), limit) else {
        return;
    };

    if expected_request.is_none() {
        assert!(
            matches!(result, Err(ResolveError::TooLong)),
            "a query over the limit was not refused: {result:?}"
        );
        return;
    }
    let understood = match (&wanted, &result) {
        (Outcome::Line(line), Ok(got)) => {
            assert_eq!(
                ResolveAnswer::from_line(line).as_ref().ok(),
                Some(got),
                "the answer is not the first line of the reply"
            );
            if line.len() == exchange.reply.len()
                && let Some(intended) = &exchange.intended
            {
                assert_eq!(got, intended, "the answer the host wrote");
            }
            true
        }
        (Outcome::Line(line), Err(ResolveError::Invalid(_))) => {
            assert!(
                ResolveAnswer::from_line(line).is_err(),
                "a line that reads as an answer was refused"
            );
            true
        }
        (Outcome::TooLong, Err(ResolveError::TooLong))
        | (Outcome::Empty, Err(ResolveError::NoAnswer))
        | (Outcome::Quiet, Err(ResolveError::Timeout(_))) => true,
        (Outcome::Cut, Err(ResolveError::Io(err))) => {
            err.kind() == std::io::ErrorKind::UnexpectedEof
        }
        _ => false,
    };
    assert!(
        understood,
        "expected {wanted:?} for the reply, got {result:?}"
    );
}
