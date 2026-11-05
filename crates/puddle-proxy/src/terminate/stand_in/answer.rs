// SPDX-License-Identifier: GPL-3.0-or-later
//! Answers: a real value that comes back from one of its hosts goes to the guest as the stand-in.
//!
//! A host a stand-in is for sees the real value, and it can hand it back: an error page or a debug
//! endpoint that echoes the request, a redirect that repeats the URL, a token in a cookie. The
//! answer of every terminated request to such a host is read on its way to the guest, headers and
//! body, and each real value in it is replaced by the stand-in the workspace already holds, so the
//! guest sees exactly what it sent and a tool that checks a token's shape finds the shape it knows.
//!
//! - **Forms**: the value as it is, percent-encoded (as a whole component, either hex case, the
//!   ways Puddle encodes a path or query secret, a form's `+` for a space), escaped for a JSON
//!   string (with or without `\/`, ASCII-only, HTML-safe) and base64-encoded (standard and URL
//!   alphabets, with or without padding). Each is replaced by the stand-in in the same form. The
//!   value inside a longer base64 text (an echoed `Basic` header in a body) cannot be replaced,
//!   because the stand-in's other length moves every character after it: it is found at each of
//!   the three alignments and the answer is cut off there, before any of it reaches the guest.
//!   In a header a `Basic` value is decoded, scrubbed and encoded again.
//! - **Streaming**: the body is not held. Only a tail that could still become a match (a suffix
//!   that is the start of some form) waits for the next piece, so the look-behind is bounded by
//!   the longest form, and a piece that ends in ordinary text (an event of a stream) goes out
//!   whole. A value split across pieces, chunks or frames is found as if the body came in one.
//! - **Encodings**: gzip, deflate, Brotli and zstd are decoded and the guest gets the plain body
//!   (`Content-Encoding` and `Content-Length` go); the request's `Accept-Encoding` is limited to
//!   those, and an answer in any other encoding is refused.
//! - **Length**: when every form has the length of its stand-in (a captured login's stand-in
//!   keeps the token's length and alphabet) and nothing is decoded, `Content-Length` stays.

use std::cmp::Reverse;
use std::io::{self, Write as _};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::Duration;

use ::http::header::{self, HeaderMap, HeaderValue};
use ::http::{Response, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use memchr::memmem::Finder;
use puddle_types::Host;
use zeroize::Zeroizing;

use super::super::exchange::AnswerBody;
use super::super::leg::BoxError;
use super::target::{encoded, keeps_in_path, keeps_in_query};
use super::{LENIENT_BASE64, StandIns, basic_credentials};
use crate::proxy::Refusal;

/// A real value shorter than this is not looked for inside longer base64 text: its encoded core
/// would be short enough to turn up in unrelated base64 data and cut answers off for nothing.
const MIN_CORE_VALUE: usize = 12;

/// How many compressed bytes are decoded at once, so one piece of a body never inflates
/// without bound before it is scrubbed.
const DECODE_STEP: usize = 4096;

/// One form of a real value and what replaces it.
struct Pattern {
    find: Zeroizing<Vec<u8>>,
    /// `None`: the form cannot be replaced, and an answer that holds it is cut off.
    put: Option<Vec<u8>>,
    /// Index into [`Scrubber::ids`].
    id: usize,
}

/// The real values an answer from one host may hold, in every form looked for.
pub(crate) struct Scrubber {
    patterns: Vec<Pattern>,
    ids: Vec<String>,
    /// Which bytes some form starts with.
    starts: [bool; 256],
    longest: usize,
    /// Every replaceable form has its stand-in's length, so a body keeps its length.
    same_length: bool,
}

/// An answer held a real value in a form that cannot be replaced (the id of its stand-in).
#[derive(Debug)]
pub(crate) struct Withheld(String);

impl std::fmt::Display for Withheld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the answer held the real value of {} in a form Puddle cannot replace",
            self.0
        )
    }
}

impl std::error::Error for Withheld {}

/// What the scrub of a connection's answers did, for its record.
#[derive(Debug, Default)]
pub(crate) struct ScrubNotes {
    ids: Mutex<Vec<String>>,
    withheld: AtomicBool,
}

impl ScrubNotes {
    fn note(&self, id: &str) {
        let mut ids = self.ids.lock().unwrap_or_else(PoisonError::into_inner);
        if !ids.iter().any(|known| known == id) {
            ids.push(id.to_owned());
        }
    }

    /// The ids of the stand-ins put back so far, each once.
    pub(crate) fn ids(&self) -> Vec<String> {
        self.ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Whether an answer was cut off because it held a value that cannot be replaced.
    pub(crate) fn withheld(&self) -> bool {
        self.withheld.load(Ordering::SeqCst)
    }
}

impl StandIns {
    /// What an answer from `host` is scrubbed with: the forms of the real value of every
    /// stand-in that is for `host`, wherever it is placed. `None` when no stand-in is.
    pub(crate) fn answer_scrubber(&self, host: &Host) -> Option<Scrubber> {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let mut patterns: Vec<Pattern> = Vec::new();
        let mut ids = Vec::new();
        for entry in guard.iter().filter(|entry| entry.hosts.contains(host)) {
            let id = ids.len();
            ids.push(entry.id());
            let real = entry.real.expose();
            for (find, put) in forms(real, &entry.text) {
                add(&mut patterns, find, Some(put), id);
            }
            for core in base64_cores(real) {
                add(&mut patterns, core, None, id);
            }
        }
        if patterns.is_empty() {
            return None;
        }
        // The longest first, so that at one place the longest form wins.
        patterns.sort_by_key(|pattern| Reverse(pattern.find.len()));
        let mut starts = [false; 256];
        for pattern in &patterns {
            if let Some(&first) = pattern.find.first()
                && let Some(slot) = starts.get_mut(usize::from(first))
            {
                *slot = true;
            }
        }
        let longest = patterns.first().map_or(0, |pattern| pattern.find.len());
        let same_length = patterns.iter().all(|pattern| {
            pattern
                .put
                .as_ref()
                .is_none_or(|put| put.len() == pattern.find.len())
        });
        Some(Scrubber {
            patterns,
            ids,
            starts,
            longest,
            same_length,
        })
    }

    /// Whether answers from `host` are scrubbed: a stand-in is for it.
    pub(crate) fn scrubs_answers_from(&self, host: &Host) -> bool {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|entry| entry.hosts.contains(host))
    }
}

/// Adds a form, unless it is empty or known already (a replaceable form wins over the same bytes
/// as one that cannot be replaced).
fn add(patterns: &mut Vec<Pattern>, find: Zeroizing<Vec<u8>>, put: Option<Vec<u8>>, id: usize) {
    if find.is_empty() {
        return;
    }
    if let Some(known) = patterns.iter_mut().find(|known| known.find == find) {
        if known.put.is_none() && put.is_some() {
            known.put = put;
            known.id = id;
        }
        return;
    }
    patterns.push(Pattern { find, put, id });
}

/// One way to write a value down.
type Encoding<'a> = &'a dyn Fn(&str) -> Zeroizing<Vec<u8>>;

/// Every form of `real` looked for, each with `stand_in` in the same form.
fn forms(real: &str, stand_in: &str) -> Vec<(Zeroizing<Vec<u8>>, Vec<u8>)> {
    let encodings: [Encoding<'_>; 15] = [
        &|text| Zeroizing::new(text.as_bytes().to_vec()),
        &|text| percent(text, false, false),
        &|text| percent(text, true, false),
        &|text| percent(text, false, true),
        &|text| encoded(text, keeps_in_path),
        &|text| encoded(text, keeps_in_query),
        &|text| json(text, Json::default()),
        &|text| {
            json(
                text,
                Json {
                    slash: true,
                    ..Json::default()
                },
            )
        },
        &|text| {
            json(
                text,
                Json {
                    ascii: true,
                    ..Json::default()
                },
            )
        },
        &|text| {
            json(
                text,
                Json {
                    html: true,
                    ..Json::default()
                },
            )
        },
        &|text| {
            json(
                text,
                Json {
                    slash: true,
                    ascii: true,
                    html: true,
                },
            )
        },
        &|text| Zeroizing::new(STANDARD.encode(text).into_bytes()),
        &|text| Zeroizing::new(STANDARD_NO_PAD.encode(text).into_bytes()),
        &|text| Zeroizing::new(URL_SAFE.encode(text).into_bytes()),
        &|text| Zeroizing::new(URL_SAFE_NO_PAD.encode(text).into_bytes()),
    ];
    let mut out: Vec<(Zeroizing<Vec<u8>>, Vec<u8>)> = Vec::new();
    for encode in encodings {
        let find = encode(real);
        if !out.iter().any(|(known, _)| *known == find) {
            out.push((find, encode(stand_in).to_vec()));
        }
    }
    out
}

/// `text` percent-encoded as a whole URL component: every byte but the unreserved ones escaped,
/// in lower-case hex if `lower`, a space as `+` if `plus` (a form).
fn percent(text: &str, lower: bool, plus: bool) -> Zeroizing<Vec<u8>> {
    let hex: &[u8; 16] = if lower {
        b"0123456789abcdef"
    } else {
        b"0123456789ABCDEF"
    };
    let mut out = Zeroizing::new(Vec::with_capacity(text.len() * 3));
    for &byte in text.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte);
        } else if plus && byte == b' ' {
            out.push(b'+');
        } else {
            out.extend_from_slice(&[
                b'%',
                hex.get(usize::from(byte >> 4)).copied().unwrap_or(b'0'),
                hex.get(usize::from(byte & 0xf)).copied().unwrap_or(b'0'),
            ]);
        }
    }
    out
}

/// How a JSON encoder escapes a string beyond what it must.
#[derive(Debug, Clone, Copy, Default)]
struct Json {
    /// `/` as `\/` (PHP, some Java encoders).
    slash: bool,
    /// Everything outside ASCII as `\uXXXX` (Python's default).
    ascii: bool,
    /// `<`, `>` and `&` as `<`, `>` and `&` (Go's default).
    html: bool,
}

/// `text` as it appears inside a JSON string written with `style`.
fn json(text: &str, style: Json) -> Zeroizing<Vec<u8>> {
    let mut out = Zeroizing::new(String::with_capacity(text.len() + 8));
    let unit = |code: u16, out: &mut String| {
        use std::fmt::Write as _;
        let _ = write!(out, "\\u{code:04x}");
    };
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '/' if style.slash => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' if style.html => unit(ch as u16, &mut out),
            c if (c as u32) < 0x20 => unit(c as u16, &mut out),
            c if style.ascii && !c.is_ascii() => {
                let mut units = [0_u16; 2];
                for code in c.encode_utf16(&mut units) {
                    unit(*code, &mut out);
                }
            }
            c => out.push(c),
        }
    }
    Zeroizing::new(out.as_bytes().to_vec())
}

/// The base64 characters that only `real` decides when it sits inside longer base64 text, at
/// each of the three byte alignments and in both alphabets.
fn base64_cores(real: &str) -> Vec<Zeroizing<Vec<u8>>> {
    let bytes = real.as_bytes();
    if bytes.len() < MIN_CORE_VALUE {
        return Vec::new();
    }
    let mut cores = Vec::new();
    for engine in [&STANDARD_NO_PAD, &URL_SAFE_NO_PAD] {
        for lead in 0..3_usize {
            let mut input = Zeroizing::new(vec![0_u8; lead]);
            input.extend_from_slice(bytes);
            input.extend_from_slice(&[0, 0]);
            let text = Zeroizing::new(engine.encode(&*input).into_bytes());
            // Character i holds bits 6i..6i+6; it is the value's own when they all are.
            let first = (8 * lead).div_ceil(6);
            let end = 8 * (lead + bytes.len()) / 6;
            if let Some(core) = text.get(first..end) {
                cores.push(Zeroizing::new(core.to_vec()));
            }
        }
    }
    cores
}

impl Scrubber {
    /// Scrubs `buf` from its start: writes to `out` what can go to the guest now and gives how
    /// many bytes of `buf` that covered. Unless `end`, a tail that could still grow into a form
    /// is left for the next call. The ids of the stand-ins put back go to `notes`.
    fn run(
        &self,
        buf: &[u8],
        end: bool,
        out: &mut Vec<u8>,
        notes: &ScrubNotes,
    ) -> Result<usize, Withheld> {
        let finders: Vec<Finder<'_>> = self
            .patterns
            .iter()
            .map(|pattern| Finder::new(&pattern.find[..]))
            .collect();
        let find_from = |index: usize, from: usize| {
            buf.get(from..)
                .and_then(|rest| finders.get(index)?.find(rest))
                .map(|at| at + from)
        };
        let mut next: Vec<Option<usize>> = (0..finders.len()).map(|i| find_from(i, 0)).collect();
        let mut copied = 0;
        let mut frontier = if end {
            buf.len()
        } else {
            self.frontier(buf, 0)
        };
        loop {
            // The leftmost match; at one place the longest (patterns are longest first).
            let best = next
                .iter()
                .enumerate()
                .filter_map(|(index, at)| Some((at.as_ref().copied()?, index)))
                .min();
            let Some((at, index)) = best else { break };
            if at >= frontier {
                break;
            }
            let Some(pattern) = self.patterns.get(index) else {
                break;
            };
            let id = self.ids.get(pattern.id).map_or("", String::as_str);
            let Some(put) = &pattern.put else {
                notes.withheld.store(true, Ordering::SeqCst);
                return Err(Withheld(id.to_owned()));
            };
            out.extend_from_slice(buf.get(copied..at).unwrap_or_default());
            out.extend_from_slice(put);
            copied = at + pattern.find.len();
            notes.note(id);
            if frontier < copied {
                frontier = self.frontier(buf, copied);
            }
            for (i, slot) in next.iter_mut().enumerate() {
                if slot.is_some_and(|at| at < copied) {
                    *slot = find_from(i, copied);
                }
            }
        }
        let upto = frontier.max(copied);
        out.extend_from_slice(buf.get(copied..upto).unwrap_or_default());
        Ok(upto)
    }

    /// The first place at or after `from` where the rest of `buf` is the start of a form (and
    /// could become one with more bytes), or the end of `buf`.
    fn frontier(&self, buf: &[u8], from: usize) -> usize {
        let first = from.max(buf.len().saturating_sub(self.longest.saturating_sub(1)));
        (first..buf.len())
            .find(|&at| {
                let tail = buf.get(at..).unwrap_or_default();
                tail.first().is_some_and(|&byte| {
                    self.starts.get(usize::from(byte)).copied().unwrap_or(false)
                }) && self.patterns.iter().any(|pattern| {
                    pattern.find.len() > tail.len() && pattern.find.starts_with(tail)
                })
            })
            .unwrap_or(buf.len())
    }

    /// `input` scrubbed whole, or `None` when nothing in it was replaced.
    fn scrub_whole(
        &self,
        input: &[u8],
        notes: &ScrubNotes,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, Withheld> {
        let before = notes.ids().len();
        let mut out = Zeroizing::new(Vec::with_capacity(input.len()));
        self.run(input, true, &mut out, notes)?;
        let changed = *out != input || notes.ids().len() != before;
        Ok(changed.then_some(out))
    }

    /// Scrubs the values of `headers` (a `Basic` value also decoded). A value that holds a real
    /// value that cannot be replaced is emptied.
    pub(crate) fn scrub_headers(&self, headers: &mut HeaderMap, notes: &ScrubNotes) {
        for value in headers.values_mut() {
            match self.scrub_value(value.as_bytes(), notes) {
                Ok(None) => {}
                // A form and its stand-in are visible ASCII in the places a header can carry,
                // so the value stays valid; were it not, it would go empty, never real.
                Ok(Some(new)) => {
                    *value = HeaderValue::from_bytes(&new).unwrap_or(HeaderValue::from_static(""));
                }
                Err(withheld) => {
                    tracing::info!(stand_in = %withheld.0, "an answer header held a real value that cannot be replaced and was emptied");
                    *value = HeaderValue::from_static("");
                }
            }
        }
    }

    fn scrub_value(
        &self,
        value: &[u8],
        notes: &ScrubNotes,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, Withheld> {
        let decoded = basic_credentials(value)
            .and_then(|token| LENIENT_BASE64.decode(token).ok())
            .map(Zeroizing::new);
        if let Some(decoded) = decoded
            && let Some(new) = self.scrub_whole(&decoded, notes)?
            && let Some(scheme) = value.get(..5)
        {
            let mut out = Zeroizing::new(scheme.to_vec());
            out.push(b' ');
            out.extend_from_slice(STANDARD.encode(&*new).as_bytes());
            return Ok(Some(out));
        }
        self.scrub_whole(value, notes)
    }
}

/// A content decoder fed pieces of an encoded body.
enum Decoder {
    Gzip(flate2::write::MultiGzDecoder<Vec<u8>>),
    /// Waits for the first two bytes to tell zlib (what the standard says `deflate` is) from a
    /// raw deflate stream (what some servers send).
    DeflateStart(Vec<u8>),
    Zlib(flate2::write::ZlibDecoder<Vec<u8>>),
    Deflate(flate2::write::DeflateDecoder<Vec<u8>>),
    Brotli(Box<brotli_decompressor::DecompressorWriter<Vec<u8>>>),
    Zstd(zstd::stream::write::Decoder<'static, Vec<u8>>),
}

impl Decoder {
    /// The decoder for a `Content-Encoding` value; `Ok(None)` for none or `identity`, `Err` for
    /// one Puddle does not read (an unknown one, or several in a row).
    fn for_encoding(value: Option<&HeaderValue>) -> Result<Option<Self>, ()> {
        let Some(value) = value else { return Ok(None) };
        let text = value.to_str().map_err(|_| ())?.trim().to_ascii_lowercase();
        let codings: Vec<&str> = text
            .split(',')
            .map(str::trim)
            .filter(|coding| !coding.is_empty() && *coding != "identity")
            .collect();
        match codings.as_slice() {
            [] => Ok(None),
            ["gzip" | "x-gzip"] => Ok(Some(Self::Gzip(flate2::write::MultiGzDecoder::new(
                Vec::new(),
            )))),
            ["deflate"] => Ok(Some(Self::DeflateStart(Vec::new()))),
            ["br"] => Ok(Some(Self::Brotli(Box::new(
                brotli_decompressor::DecompressorWriter::new(Vec::new(), DECODE_STEP),
            )))),
            ["zstd"] => zstd::stream::write::Decoder::new(Vec::new())
                .map(|decoder| Some(Self::Zstd(decoder)))
                .map_err(|_| ()),
            _ => Err(()),
        }
    }

    /// Decodes `input`, appending the plain bytes to `out`.
    fn push(&mut self, input: &[u8], out: &mut Vec<u8>) -> io::Result<()> {
        if let Self::DeflateStart(held) = self {
            held.extend_from_slice(input);
            let [cmf, flg, ..] = held[..] else {
                return Ok(());
            };
            let zlib = cmf & 0x0f == 8 && (u16::from(cmf) * 256 + u16::from(flg)) % 31 == 0;
            let held = std::mem::take(held);
            *self = if zlib {
                Self::Zlib(flate2::write::ZlibDecoder::new(Vec::new()))
            } else {
                Self::Deflate(flate2::write::DeflateDecoder::new(Vec::new()))
            };
            return self.push(&held, out);
        }
        for piece in input.chunks(DECODE_STEP) {
            self.writer().write_all(piece)?;
            self.writer().flush()?;
            out.append(self.output());
        }
        Ok(())
    }

    /// Ends the body: what the decoder still holds, or an error when the body stopped short.
    fn finish(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        match self {
            Self::DeflateStart(held) if held.is_empty() => return Ok(()),
            Self::DeflateStart(_) => return Err(io::Error::other("a deflate body ended at once")),
            Self::Gzip(decoder) => decoder.try_finish()?,
            Self::Zlib(decoder) => decoder.try_finish()?,
            Self::Deflate(decoder) => decoder.try_finish()?,
            Self::Brotli(decoder) => decoder.close()?,
            Self::Zstd(decoder) => decoder.flush()?,
        }
        out.append(self.output());
        Ok(())
    }

    fn writer(&mut self) -> &mut dyn io::Write {
        match self {
            Self::Gzip(decoder) => decoder,
            Self::Zlib(decoder) => decoder,
            Self::Deflate(decoder) => decoder,
            Self::Brotli(decoder) => &mut **decoder,
            Self::Zstd(decoder) => decoder,
            Self::DeflateStart(held) => held,
        }
    }

    fn output(&mut self) -> &mut Vec<u8> {
        match self {
            Self::Gzip(decoder) => decoder.get_mut(),
            Self::Zlib(decoder) => decoder.get_mut(),
            Self::Deflate(decoder) => decoder.get_mut(),
            Self::Brotli(decoder) => decoder.get_mut(),
            Self::Zstd(decoder) => decoder.get_mut(),
            Self::DeflateStart(held) => held,
        }
    }
}

/// An answer's body on its way to the guest with the real values put back as stand-ins.
pub(crate) struct ScrubBody {
    inner: AnswerBody,
    scrubber: Scrubber,
    decoder: Option<Decoder>,
    /// What came but could still be the start of a form; it may hold part of a real value.
    held: Zeroizing<Vec<u8>>,
    notes: Arc<ScrubNotes>,
    trailers: Option<HeaderMap>,
    ended: bool,
}

impl ScrubBody {
    /// Takes `plain` (decoded body bytes) and gives what can go to the guest now.
    fn step(&mut self, plain: &[u8], end: bool) -> Result<Bytes, BoxError> {
        self.held.extend_from_slice(plain);
        let mut out = Vec::with_capacity(self.held.len());
        let used = self.scrubber.run(&self.held, end, &mut out, &self.notes)?;
        self.held.drain(..used);
        Ok(Bytes::from(out))
    }

    fn decode(&mut self, data: &[u8], end: bool) -> Result<Zeroizing<Vec<u8>>, BoxError> {
        let Some(decoder) = self.decoder.as_mut() else {
            return Ok(Zeroizing::new(data.to_vec()));
        };
        let mut plain = Zeroizing::new(Vec::new());
        decoder
            .push(data, &mut plain)
            .map_err(|err| undecodable(&err))?;
        if end {
            decoder
                .finish(&mut plain)
                .map_err(|err| undecodable(&err))?;
        }
        Ok(plain)
    }
}

fn undecodable(err: &io::Error) -> BoxError {
    Box::new(io::Error::other(format!(
        "the answer's encoding could not be decoded: {err}"
    )))
}

impl Body for ScrubBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        loop {
            if this.ended {
                return Poll::Ready(this.trailers.take().map(|t| Ok(Frame::trailers(t))));
            }
            let polled = match Pin::new(&mut this.inner).poll_frame(cx) {
                Poll::Ready(polled) => polled,
                Poll::Pending => return Poll::Pending,
            };
            let out = match polled {
                Some(Ok(frame)) => match frame.into_data() {
                    Ok(data) => this
                        .decode(&data, false)
                        .and_then(|plain| this.step(&plain, false)),
                    Err(frame) => {
                        if let Ok(mut trailers) = frame.into_trailers() {
                            this.scrubber.scrub_headers(&mut trailers, &this.notes);
                            this.trailers = Some(trailers);
                        }
                        continue;
                    }
                },
                Some(Err(err)) => return Poll::Ready(Some(Err(err))),
                None => {
                    this.ended = true;
                    this.decode(&[], true)
                        .and_then(|plain| this.step(&plain, true))
                }
            };
            match out {
                Ok(bytes) if bytes.is_empty() => {}
                Ok(bytes) => return Poll::Ready(Some(Ok(Frame::data(bytes)))),
                Err(err) => {
                    this.ended = true;
                    this.trailers = None;
                    return Poll::Ready(Some(Err(err)));
                }
            }
        }
    }

    fn is_end_stream(&self) -> bool {
        self.ended && self.trailers.is_none()
    }

    fn size_hint(&self) -> SizeHint {
        if self.decoder.is_none() && self.scrubber.same_length {
            self.inner.size_hint()
        } else {
            SizeHint::new()
        }
    }
}

/// The record's reason for an answer cut off because it held a real value that cannot be
/// replaced.
pub(crate) const ANSWER_WITHHELD: &str = "answer_withheld";

/// The guest's answer when the server's is in an encoding Puddle cannot read, so cannot check
/// for real values.
pub(crate) fn answer_encoding_refusal(host: &Host) -> Refusal {
    Refusal::new(
        "502 Bad Gateway",
        format!(
            "{host} answered in a content encoding Puddle cannot read, so it could not check the answer for secret values (gzip, deflate, br and zstd are read)"
        ),
    )
    .coded("answer_encoding")
}

/// The guest's answer when the server's answer, held to be scrubbed, did not arrive whole.
pub(crate) fn answer_failed_refusal(host: &Host) -> Refusal {
    Refusal::new(
        "502 Bad Gateway",
        format!("{host} did not finish its answer"),
    )
}

/// Why an answer is not passed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NotScrubbed {
    /// It is encoded in a way Puddle does not decode.
    Encoding,
    /// Its body, held to be scrubbed whole, failed, stalled or ran over its length.
    Failed,
}

/// Prepares `response`, an answer from `host`, for the guest: its headers scrubbed now, its body
/// as it streams (decoded first when it is encoded). `no_body`: the answer carries no body
/// (`HEAD`, `1xx`, `204`, `304`), so its encoding does not matter. The answer is unchanged when
/// no stand-in is for `host`. A plain body of a known length up to [`MAX_HELD`] is read first
/// (`idle` bounds each wait) and scrubbed whole, so the guest gets it with an exact length even
/// when a stand-in is longer than its value.
pub(crate) async fn scrub_answer(
    stand_ins: &StandIns,
    response: Response<AnswerBody>,
    host: &Host,
    no_body: bool,
    notes: &Arc<ScrubNotes>,
    idle: Duration,
) -> Result<Response<AnswerBody>, NotScrubbed> {
    let Some(scrubber) = stand_ins.answer_scrubber(host) else {
        return Ok(response);
    };
    let (mut parts, inner) = response.into_parts();
    scrubber.scrub_headers(&mut parts.headers, notes);
    let no_body = no_body
        || parts.status.is_informational()
        || parts.status == StatusCode::NO_CONTENT
        || parts.status == StatusCode::NOT_MODIFIED;
    if no_body {
        return Ok(Response::from_parts(parts, inner));
    }
    let decoder = Decoder::for_encoding(parts.headers.get(header::CONTENT_ENCODING))
        .map_err(|()| NotScrubbed::Encoding)?;
    let declared = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok()?.trim().parse::<u64>().ok());
    let mut inner = inner;
    if decoder.is_none()
        && !scrubber.same_length
        && declared.is_some_and(|length| length <= MAX_HELD as u64)
        && let AnswerBody::Streaming(_) = &inner
    {
        inner = AnswerBody::Buffered(Some(hold(&mut inner, idle).await?));
    }
    // A body read in full (a short one above, a token exchange's answer) is scrubbed now and
    // keeps an exact length; one that must be cut off goes the streaming way, which cuts it.
    if decoder.is_none()
        && let AnswerBody::Buffered(Some(bytes)) = &inner
        && let Ok(new) = scrubber.scrub_whole(bytes, notes)
    {
        let body = new.map_or_else(|| bytes.clone(), |new| Bytes::copy_from_slice(&new));
        if let Ok(length) = HeaderValue::from_str(&body.len().to_string()) {
            parts.headers.insert(header::CONTENT_LENGTH, length);
        }
        return Ok(Response::from_parts(
            parts,
            AnswerBody::Buffered(Some(body)),
        ));
    }
    if decoder.is_some() {
        parts.headers.remove(header::CONTENT_ENCODING);
    }
    if decoder.is_some() || !scrubber.same_length {
        parts.headers.remove(header::CONTENT_LENGTH);
    }
    let body = ScrubBody {
        inner,
        scrubber,
        decoder,
        held: Zeroizing::new(Vec::new()),
        notes: Arc::clone(notes),
        trailers: None,
        ended: false,
    };
    Ok(Response::from_parts(
        parts,
        AnswerBody::Scrubbed(Box::new(body)),
    ))
}

/// The longest body of a known length that is held and scrubbed whole, so that it keeps a
/// length; a longer one streams and goes to the guest without one.
pub(crate) const MAX_HELD: usize = 64 * 1024;

/// Reads `body` to its end (at most [`MAX_HELD`] bytes). Trailers of such a body are not kept.
async fn hold(body: &mut AnswerBody, idle: Duration) -> Result<Bytes, NotScrubbed> {
    use http_body_util::BodyExt as _;
    let mut whole = Vec::new();
    loop {
        let frame = tokio::time::timeout(idle, body.frame())
            .await
            .map_err(|_| NotScrubbed::Failed)?;
        let Some(frame) = frame else { break };
        let frame = frame.map_err(|_| NotScrubbed::Failed)?;
        if let Ok(data) = frame.into_data() {
            whole.extend_from_slice(&data);
            if whole.len() > MAX_HELD {
                return Err(NotScrubbed::Failed);
            }
        }
    }
    Ok(Bytes::from(whole))
}

/// The content codings an answer may come in when it is scrubbed.
const READABLE_CODINGS: [&str; 6] = ["gzip", "x-gzip", "deflate", "br", "zstd", "identity"];

/// Limits a request's `Accept-Encoding` to the codings Puddle decodes, for a request whose
/// answer is scrubbed: the server then picks one of those (or none).
pub(crate) fn limit_accept_encoding(headers: &mut HeaderMap) {
    let Some(value) = headers.get(header::ACCEPT_ENCODING) else {
        return;
    };
    let text = value.to_str().unwrap_or_default();
    let kept: Vec<&str> = text
        .split(',')
        .map(str::trim)
        .filter(|item| {
            let coding = item.split(';').next().unwrap_or_default().trim();
            READABLE_CODINGS
                .iter()
                .any(|known| known.eq_ignore_ascii_case(coding))
        })
        .collect();
    match HeaderValue::from_str(&kept.join(", ")) {
        Ok(new) if !kept.is_empty() => {
            headers.insert(header::ACCEPT_ENCODING, new);
        }
        _ => {
            headers.remove(header::ACCEPT_ENCODING);
        }
    }
}

#[cfg(test)]
#[path = "answer_tests.rs"]
mod tests;
