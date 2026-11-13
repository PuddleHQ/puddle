// SPDX-License-Identifier: GPL-3.0-or-later
//! Tokens in an answer that Puddle does not hold.
//!
//! A tool that logs in without a capture profile, or a server that hands out a token of its own
//! accord, puts a real credential in the workspace without Puddle ever seeing it. Puddle cannot
//! swap what it does not know, but it can say that it went by: every decrypted answer is read, as
//! the guest sees it, for tokens of a known kind, and a find raises the warning of
//! [`ForeignCredentialWarning`](super::super::ForeignCredentialWarning) with the host and the
//! kind (never the value). Nothing in the answer changes: this only observes.
//!
//! - **Kinds**: a GitHub token with a valid checksum, a fine-grained GitHub token, a GitLab
//!   token, an npm token with a valid checksum, an 84-character Azure DevOps token (`AZDO` at 76),
//!   an Anthropic key (`sk-ant-`), an `OpenAI` project key (`sk-proj-`), and a JSON Web Token in an
//!   answer that also has a `refresh_token` field (a sign-in's answer; a JWT alone is too common
//!   to say anything).
//! - **What counts as held**: a run that is the text of a stand-in, or the real value of an
//!   entry, of the workspace's registry is Puddle's own and never found. A `refresh_token`
//!   field that holds a stand-in is the capture's own work and does not pair with a JWT.
//! - **Where**: every header value, and the body when its type is text (or has none), decoded
//!   when it is gzip, deflate, Brotli or zstd. A body of another coding or type is not read. At
//!   most [`MAX_SCANNED`] bytes of a body are read, so a large answer costs a bounded amount.
//! - **Streaming**: a token is a maximal run of letters, digits, `_`, `-` and `.`. The body is
//!   read piece by piece and only the end of a run that could still grow (and a `refresh_token`
//!   name waiting for its value) is kept for the next piece, so a token split across pieces is
//!   found as if the body came in one, and the memory held is at most [`MAX_RUN`] bytes.

use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context as TaskContext, Poll};

use ::http::header::{self, HeaderValue};
use ::http::{Response, StatusCode};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use puddle_types::{EventSink, Host, UnknownTokenKind, WorkspaceId};
use zeroize::Zeroizing;

use super::super::exchange::AnswerBody;
use super::super::foreign;
use super::super::leg::BoxError;
use super::StandIns;
use super::answer::{Decoder, Pushed};
use crate::terminate::{Context, ForeignCredentialWarning};

/// The longest run read as a token: a JSON Web Token is the longest kind, and a longer run is
/// not one.
const MAX_RUN: usize = 4096;

/// The most bytes of one answer's body (after decoding) that are read.
pub(crate) const MAX_SCANNED: usize = 1024 * 1024;

const REFRESH_FIELD: &[u8] = b"refresh_token";
const ANTHROPIC_PREFIX: &[u8] = b"sk-ant-";
const OPENAI_PREFIX: &[u8] = b"sk-proj-";
const MIN_API_KEY_BODY: usize = 40;
/// The most bytes of quotes, escapes and white space allowed between a field's name, its
/// separator and its value.
const MAX_GAP: usize = 6;

fn is_run_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.')
}

/// What a run of token characters is.
enum Found {
    Kind(UnknownTokenKind),
    Jwt,
}

/// The kind of a whole run, when its bytes say.
fn read_run(run: &[u8]) -> Option<Found> {
    if foreign::is_github_checksummed(run) {
        Some(Found::Kind(UnknownTokenKind::GithubToken))
    } else if foreign::is_fine_grained(run) {
        Some(Found::Kind(UnknownTokenKind::GithubFineGrainedToken))
    } else if foreign::is_npm(run) {
        Some(Found::Kind(UnknownTokenKind::NpmToken))
    } else if foreign::is_gitlab(run) {
        Some(Found::Kind(UnknownTokenKind::GitlabToken))
    } else if foreign::is_azure_devops(run) {
        Some(Found::Kind(UnknownTokenKind::AzureDevopsToken))
    } else if is_api_key(run, ANTHROPIC_PREFIX) {
        Some(Found::Kind(UnknownTokenKind::AnthropicKey))
    } else if is_api_key(run, OPENAI_PREFIX) {
        Some(Found::Kind(UnknownTokenKind::OpenaiKey))
    } else if is_jwt(run) {
        Some(Found::Jwt)
    } else {
        None
    }
}

/// A prefix and a long body of base64url characters.
fn is_api_key(run: &[u8], prefix: &[u8]) -> bool {
    run.strip_prefix(prefix)
        .is_some_and(|body| body.len() >= MIN_API_KEY_BODY && body.iter().all(|b| *b != b'.'))
}

/// Three base64url segments, the first the encoding of a JSON object.
fn is_jwt(run: &[u8]) -> bool {
    if !run.starts_with(b"eyJ") {
        return false;
    }
    let mut parts = run.split(|b| *b == b'.');
    let (Some(head), Some(claims), Some(signature), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    !claims.is_empty()
        && !signature.is_empty()
        && URL_SAFE_NO_PAD
            .decode(head)
            .is_ok_and(|decoded| decoded.first() == Some(&b'{'))
}

/// What follows a `refresh_token` name.
enum Value<'a> {
    /// The value, a run of token characters (possibly none).
    Run(&'a [u8]),
    /// The text ends before the value is complete: ask again with more.
    Pending,
    /// It is not a field (no separator).
    NotAField,
}

/// The value after the name that ends at `at` in `buf`: optional quotes and escapes, a `:` or
/// `=`, optional quotes, then a run. `end`: no more bytes will come.
fn field_value(buf: &[u8], at: usize, end: bool) -> Value<'_> {
    let pending = if end {
        Value::NotAField
    } else {
        Value::Pending
    };
    let mut at = at;
    for step in 0..2 {
        let gap = buf
            .get(at..)
            .unwrap_or_default()
            .iter()
            .take_while(|b| matches!(b, b'"' | b'\\' | b' ' | b'\t'))
            .count();
        if gap > MAX_GAP {
            return Value::NotAField;
        }
        at += gap;
        if step == 0 {
            match buf.get(at) {
                Some(b':' | b'=') => at += 1,
                Some(_) => return Value::NotAField,
                None => return pending,
            }
        }
    }
    let run = buf
        .get(at..)
        .unwrap_or_default()
        .iter()
        .take_while(|b| is_run_byte(**b))
        .count();
    if at + run == buf.len() && !end {
        return Value::Pending;
    }
    Value::Run(buf.get(at..at + run).unwrap_or_default())
}

/// Reads an answer piece by piece for tokens of known kinds.
#[derive(Default)]
pub(crate) struct TokenScanner {
    /// The end of what came that may still change: a run that could grow, from a
    /// `refresh_token` name waiting for its value if that is earlier.
    carry: Zeroizing<Vec<u8>>,
    /// The run at the end of the last piece was too long to be a token and is being skipped.
    skipping: bool,
    scanned: usize,
    jwt: bool,
    refresh: bool,
    told: Vec<UnknownTokenKind>,
}

impl TokenScanner {
    /// How many more bytes of the answer are read.
    fn room(&self) -> usize {
        MAX_SCANNED.saturating_sub(self.scanned)
    }

    /// Reads `data`, the next bytes of the answer (`end`: the last ones), and gives the kinds
    /// found for the first time. `holds` says whether a run is Puddle's own (never found).
    pub(crate) fn feed(
        &mut self,
        data: &[u8],
        end: bool,
        holds: &dyn Fn(&[u8]) -> bool,
    ) -> Vec<UnknownTokenKind> {
        let room = self.room();
        let capped = data.len() >= room;
        let data = data.get(..data.len().min(room)).unwrap_or_default();
        self.scanned += data.len();
        let end = end || capped;
        let mut data = data;
        if self.skipping {
            let run = data.iter().take_while(|b| is_run_byte(**b)).count();
            data = data.get(run..).unwrap_or_default();
            self.skipping = data.is_empty() && !end;
        }
        let mut buf = Zeroizing::new(std::mem::take(&mut *self.carry));
        buf.extend_from_slice(data);
        let (mut found, keep) = self.read(&buf, end, holds);
        if self.jwt && self.refresh {
            self.note(UnknownTokenKind::JwtWithRefreshToken, &mut found);
        }
        self.carry = Zeroizing::new(buf.get(keep..).unwrap_or_default().to_vec());
        if capped {
            self.scanned = MAX_SCANNED;
        }
        found
    }

    /// Reads all of `buf`: the kinds found, and where the part to keep for the next piece begins.
    fn read(
        &mut self,
        buf: &[u8],
        end: bool,
        holds: &dyn Fn(&[u8]) -> bool,
    ) -> (Vec<UnknownTokenKind>, usize) {
        let mut found = Vec::new();
        let mut keep = buf.len();
        let mut at = 0;
        while let Some(&byte) = buf.get(at) {
            if !is_run_byte(byte) {
                at += 1;
                continue;
            }
            let start = at;
            at += buf
                .get(at..)
                .unwrap_or_default()
                .iter()
                .take_while(|b| is_run_byte(**b))
                .count();
            let run = buf.get(start..at).unwrap_or_default();
            if at == buf.len() && !end {
                if run.len() > MAX_RUN {
                    self.skipping = true;
                } else {
                    keep = keep.min(start);
                }
                break;
            }
            if run.len() > MAX_RUN {
                continue;
            }
            if run == REFRESH_FIELD {
                match field_value(buf, at, end) {
                    Value::Run(value) => {
                        if !value.is_empty() && value != b"null" && !holds(value) {
                            self.refresh = true;
                        }
                    }
                    Value::Pending => keep = keep.min(start),
                    Value::NotAField => {}
                }
            } else if let Some(read) = read_run(run)
                && !holds(run)
            {
                match read {
                    Found::Kind(kind) => self.note(kind, &mut found),
                    Found::Jwt => self.jwt = true,
                }
            }
        }
        (found, keep)
    }

    fn note(&mut self, kind: UnknownTokenKind, found: &mut Vec<UnknownTokenKind>) {
        if !self.told.contains(&kind) {
            self.told.push(kind);
            found.push(kind);
        }
    }
}

impl StandIns {
    /// Whether `run` is the text of a stand-in or the real value of an entry: Puddle's own.
    fn holds_token(&self, run: &[u8]) -> bool {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .any(|entry| entry.text.as_bytes() == run || entry.real.expose().as_bytes() == run)
    }
}

/// Where a connection's findings go: the warning, the event sink and the record's list.
pub(crate) struct AnswerReport {
    warning: Arc<ForeignCredentialWarning>,
    sink: Arc<dyn EventSink>,
    workspace: WorkspaceId,
    host: Host,
    flagged: Mutex<Vec<String>>,
}

impl AnswerReport {
    /// The report of a connection: its workspace and host, the proxy's sink and the workspace's
    /// warning.
    pub(crate) fn of(cx: &Context) -> Self {
        Self::new(
            Arc::clone(cx.termination.foreign_warning()),
            Arc::clone(&cx.proxy.sink),
            cx.workspace.clone(),
            cx.target.host.clone(),
        )
    }

    fn new(
        warning: Arc<ForeignCredentialWarning>,
        sink: Arc<dyn EventSink>,
        workspace: WorkspaceId,
        host: Host,
    ) -> Self {
        Self {
            warning,
            sink,
            workspace,
            host,
            flagged: Mutex::new(Vec::new()),
        }
    }

    fn note(&self, kind: UnknownTokenKind) {
        let mut flagged = self.flagged.lock().unwrap_or_else(PoisonError::into_inner);
        self.warning.note_answer(
            &self.workspace,
            &self.host,
            self.sink.as_ref(),
            &mut flagged,
            kind,
        );
    }

    /// The kinds found so far, each once, for the connection's record.
    pub(crate) fn kinds(&self) -> Vec<String> {
        self.flagged
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// An answer's body, read for tokens as it passes unchanged.
pub(crate) struct ScanBody {
    inner: AnswerBody,
    scanner: TokenScanner,
    decoder: Option<Decoder>,
    stand_ins: Arc<StandIns>,
    report: Arc<AnswerReport>,
    /// Reading stopped: the cap, a body that cannot be decoded, or the end.
    done: bool,
}

impl ScanBody {
    fn scan(&mut self, plain: &[u8], end: bool) {
        let stand_ins = Arc::clone(&self.stand_ins);
        for kind in self
            .scanner
            .feed(plain, end, &|run| stand_ins.holds_token(run))
        {
            self.report.note(kind);
        }
    }

    /// Reads one piece of the body as it is on the wire (`None`: the body ended). Past the cap
    /// nothing is copied or decoded: the decoder is dropped and later pieces pass untouched.
    fn read(&mut self, data: Option<&[u8]>) {
        if self.done {
            return;
        }
        let end = data.is_none();
        let data = data.unwrap_or_default();
        let room = self.scanner.room();
        let plain = match self.decoder.as_mut() {
            None => Zeroizing::new(
                data.get(..data.len().min(room))
                    .unwrap_or_default()
                    .to_vec(),
            ),
            Some(decoder) => {
                let mut plain = Zeroizing::new(Vec::new());
                let result = decoder.push(data, &mut plain, room).and_then(|pushed| {
                    if end {
                        decoder.finish(&mut plain)?;
                    }
                    Ok(pushed)
                });
                // The cap, or a fault: whatever decoded before it was read; the rest cannot be
                // (or need not be).
                if !matches!(result, Ok(Pushed::All)) {
                    self.done = true;
                }
                plain
            }
        };
        self.scan(&plain, end || self.done);
        if end || self.scanner.room() == 0 {
            self.done = true;
        }
        if self.done {
            self.decoder = None;
        }
    }
}

impl Body for ScanBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut TaskContext<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let this = &mut *self;
        let polled = Pin::new(&mut this.inner).poll_frame(cx);
        match &polled {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(data) = frame.data_ref() {
                    this.read(Some(data));
                }
            }
            Poll::Ready(None) => this.read(None),
            Poll::Ready(Some(Err(_))) | Poll::Pending => {}
        }
        polled
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

/// The check behind the cargo-fuzz target: the first byte sets a piece size, the rest is an
/// answer read whole and in pieces of that size, and the two must find the same kinds, each at
/// most once, while keeping no more than one run between pieces.
///
/// # Panics
///
/// When they differ, a kind is found twice, or the scanner keeps more than it may.
#[cfg(feature = "fuzzing")]
pub(crate) fn check(bytes: &[u8]) {
    let Some((&first, data)) = bytes.split_first() else {
        return;
    };
    let size = usize::from(first) % 97 + 1;
    // A deterministic stand-in for "Puddle holds this run".
    let holds = |run: &[u8]| run.len().is_multiple_of(2);
    let names = |kinds: &[UnknownTokenKind]| {
        let mut names: Vec<&str> = kinds.iter().map(|kind| kind.as_str()).collect();
        names.sort_unstable();
        names
    };
    let mut one = TokenScanner::default();
    let mut whole = one.feed(data, false, &holds);
    whole.extend(one.feed(&[], true, &holds));
    let mut pieces = TokenScanner::default();
    let mut split = Vec::new();
    for piece in data.chunks(size) {
        split.extend(pieces.feed(piece, false, &holds));
        assert!(pieces.carry.len() <= MAX_RUN, "more than a run was kept");
    }
    split.extend(pieces.feed(&[], true, &holds));
    assert_eq!(names(&whole), names(&split), "pieces of {size}");
    let mut unique = names(&split);
    unique.dedup();
    assert_eq!(unique.len(), split.len(), "a kind was found twice");
}

/// Whether a body of this `Content-Type` is read: text, JSON, XML, a form or a script, or no
/// type at all.
fn is_text_type(value: Option<&HeaderValue>) -> bool {
    let Some(value) = value else { return true };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let text = text.to_ascii_lowercase();
    let essence = text.split(';').next().unwrap_or_default().trim();
    essence.starts_with("text/")
        || essence.contains("json")
        || essence.contains("xml")
        || essence.contains("javascript")
        || essence == "application/x-www-form-urlencoded"
}

/// Reads `response`, an answer to the guest, for tokens Puddle does not hold: its headers now,
/// its body as it streams. The answer itself is not changed. A body that is not text, or is in a
/// coding Puddle does not decode, is not read (its headers still are). Put this after the scrub,
/// so it sees what the guest sees.
pub(crate) fn scan_answer(
    stand_ins: &Arc<StandIns>,
    response: Response<AnswerBody>,
    no_body: bool,
    report: &Arc<AnswerReport>,
) -> Response<AnswerBody> {
    let (parts, inner) = response.into_parts();
    let mut scanner = TokenScanner::default();
    {
        let holds = |run: &[u8]| stand_ins.holds_token(run);
        for value in parts.headers.values() {
            let mut line = Zeroizing::new(value.as_bytes().to_vec());
            line.push(b'\n');
            for kind in scanner.feed(&line, false, &holds) {
                report.note(kind);
            }
        }
    }
    let no_body = no_body
        || parts.status.is_informational()
        || parts.status == StatusCode::NO_CONTENT
        || parts.status == StatusCode::NOT_MODIFIED;
    let decoder = Decoder::for_encoding(parts.headers.get(header::CONTENT_ENCODING));
    let readable =
        !no_body && is_text_type(parts.headers.get(header::CONTENT_TYPE)) && decoder.is_ok();
    // Every header line ended with a newline, so nothing is pending that an end could add.
    if !readable {
        return Response::from_parts(parts, inner);
    }
    let body = ScanBody {
        inner,
        scanner,
        decoder: decoder.unwrap_or(None),
        stand_ins: Arc::clone(stand_ins),
        report: Arc::clone(report),
        done: false,
    };
    Response::from_parts(parts, AnswerBody::Scanned(Box::new(body)))
}

#[cfg(test)]
mod tests;
