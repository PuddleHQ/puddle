// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stand-ins: values a workspace holds in place of secrets.
//!
//! A workspace never holds a secret. It holds a random *stand-in* (an environment variable the
//! user set, or the token a login inside the workspace produced), and the proxy swaps the real
//! value in on the way out, on a terminated connection to a host that stand-in is for, and
//! nowhere else. That is what keeps a stand-in worthless to a workspace that sends it to its own
//! server: the swap needs the host to be on the stand-in's list.
//!
//! [`StandIns`] is the registry of one workspace, shared by everything that creates stand-ins
//! (user secrets, captured logins) and consulted by every terminated request. The rules the swap
//! follows:
//!
//! - **Where**: each stand-in has one [`Placement`]. By default that is header values: never
//!   names, the request target or a body. A value that starts with `Basic ` is decoded, swapped
//!   and encoded again, because git sends `user:stand-in` base64-encoded. Headers an injector sets
//!   are never looked at. A secret may instead opt in to one named query parameter or the path,
//!   for APIs that take a key nowhere else ([`StandIns::swap_target`]); then it is swapped there
//!   and nowhere else. A stand-in in any other place goes out unchanged and is reported.
//! - **Which hosts**: the connection's host (the one the certificate, `Host`/`:authority` and the
//!   `CONNECT` agree on) must match one of the stand-in's hosts. A stand-in seen on any other
//!   terminated host goes out unchanged and is reported for the audit. Plain HTTP and spliced
//!   connections never reach this code, so a stand-in there is unchanged too.
//! - **Whose**: only the stand-ins in this workspace's registry are known. Another workspace's
//!   stand-in (or one of a secret that was removed) is left alone, but a value that has a stand-in's
//!   shape is flagged for the audit as stale ([`stale`]).
//! - **When**: after the upstream's certificate is verified, so a server that is not accepted
//!   never sees a real value, and each request on its own, so a stand-in added while a
//!   connection is open applies from its next request.
//!
//! A real value is zeroised when its entry is dropped. The copy in a request that is on its way
//! out lives in the HTTP library's buffers and is not.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, RwLock};

use ::http::header::{HeaderMap, HeaderName, HeaderValue};
use base64::Engine as _;
use base64::alphabet;
use base64::engine::DecodePaddingMode;
use base64::engine::general_purpose::{GeneralPurpose, GeneralPurposeConfig, STANDARD};
use memchr::memmem::Finder;
use puddle_types::{ForeignCredentialKind, Host, QueryName};
use zeroize::Zeroizing;

use super::foreign;
use super::inject::{InjectedHeader, Injection, SecretValue};
use super::set::TerminationSet;
use super::stale;
use super::token_body::TokenBody;

mod answer;
mod scan;
mod target;

pub(crate) use answer::{
    ANSWER_WITHHELD, NotScrubbed, ScrubBody, ScrubNotes, answer_encoding_refusal,
    answer_failed_refusal, limit_accept_encoding, scrub_answer,
};
#[cfg(feature = "fuzzing")]
pub(crate) use scan::check as check_answer_scan;
pub(crate) use scan::{AnswerReport, ScanBody, scan_answer};

/// The shortest stand-in accepted. A short one would match inside ordinary header text and swap
/// it.
const MIN_STAND_IN_LEN: usize = 16;

/// The longest stand-in accepted: a Copilot session token is a few hundred characters, a Microsoft
/// access token is one to three thousand, and one with many group claims reaches about six thousand.
/// A header carries 64 KiB, and the credential store keeps a long value in pieces.
const MAX_LEN: usize = 8192;

/// Most random bytes of a generated stand-in: 128 bits.
const RANDOM_BYTES: usize = 16;

/// Standard base64 that accepts a missing `=` padding (some clients leave it off).
const LENIENT_BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Where in a request a stand-in is swapped for its real value. Anywhere else it goes out
/// unchanged and is reported, also toward its own hosts.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum Placement {
    /// The values of request headers (and a captured login's refresh field).
    #[default]
    Header,
    /// The value of the one query parameter with this name, when the whole value is the
    /// stand-in.
    Query(QueryName),
    /// Anywhere in the path (not the query): a path segment, or part of one (`/bot<stand-in>/`).
    Path,
}

/// Where a stand-in came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum StandInOrigin {
    /// A secret the user gave the workspace, set as an environment variable.
    Secret,
    /// A token captured from a login made inside the workspace.
    CapturedLogin,
}

impl StandInOrigin {
    fn label(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::CapturedLogin => "login",
        }
    }
}

/// Why a stand-in could not be made or registered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StandInError {
    /// A name is 1 to 64 characters of letters, digits, `_`, `-` and `.`.
    #[error("{0:?} is not a name a stand-in can have")]
    Name(String),
    /// A stand-in is at least 16 visible ASCII characters without spaces, so it cannot match
    /// ordinary header text.
    #[error("a stand-in is {MIN_STAND_IN_LEN} to {MAX_LEN} visible characters without spaces")]
    Shape,
    /// A stand-in with no host is not allowed: it would be worth nothing anywhere.
    #[error("a stand-in needs at least one host")]
    NoHosts,
    /// A `*.` pattern over a domain anyone can register a name under (`*.github.io`, `*.co.uk`):
    /// the real value would go to whoever registers one.
    #[error("*.{0} covers names anyone can register; list the exact hosts instead")]
    SharedDomain(String),
    /// The real value is empty or has characters a header cannot carry (line breaks, control
    /// characters).
    #[error("the real value is empty or has characters a header cannot carry")]
    Value,
    /// Another entry has the same stand-in, or one inside the other, which would make a swap
    /// ambiguous.
    #[error(
        "another entry has a stand-in that equals or contains this one (or the other way round)"
    )]
    Overlaps,
    /// The operating system has no randomness to give.
    #[error("the operating system has no randomness to give")]
    Random,
    /// An entry handed to [`StandIns::replace_origin`] is of another origin than the one being
    /// replaced.
    #[error("an entry is not of the origin being replaced")]
    Origin,
}

/// One stand-in and what it stands for.
pub struct StandIn {
    origin: StandInOrigin,
    name: String,
    text: String,
    /// Finds `text` in a header value.
    finder: Finder<'static>,
    real: SecretValue,
    hosts: TerminationSet,
    placement: Placement,
}

impl std::fmt::Debug for StandIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Neither the stand-in's text nor the real value: both end up in logs by accident.
        f.debug_struct("StandIn")
            .field("id", &self.id())
            .field("hosts", &self.hosts)
            .field("placement", &self.placement)
            .finish_non_exhaustive()
    }
}

fn is_header_safe(value: &str) -> bool {
    !value.is_empty() && HeaderValue::from_str(value).is_ok()
}

fn is_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}

impl StandIn {
    /// A stand-in `stand_in` for `real`, to be swapped in only toward `hosts` (exact names or
    /// `*.suffix` patterns, as in a [`TerminationSet`]).
    ///
    /// `name` labels it in logs and in the audit (the environment variable's name, or the login's
    /// name); it is never secret.
    ///
    /// # Errors
    /// [`StandInError`] for a name, stand-in or real value that is not acceptable, no hosts, or a
    /// `*.` pattern over a domain anyone can register a name under.
    pub fn new(
        origin: StandInOrigin,
        name: &str,
        stand_in: &str,
        real: SecretValue,
        hosts: TerminationSet,
    ) -> Result<Self, StandInError> {
        if !is_name(name) {
            return Err(StandInError::Name(name.to_owned()));
        }
        let shape_ok = (MIN_STAND_IN_LEN..=MAX_LEN).contains(&stand_in.len())
            && stand_in.bytes().all(|b| b.is_ascii_graphic());
        if !shape_ok {
            return Err(StandInError::Shape);
        }
        if hosts.is_empty() {
            return Err(StandInError::NoHosts);
        }
        if let Some(base) = hosts
            .wildcard_bases()
            .find(|base| psl::domain(base.as_bytes()).is_none())
        {
            return Err(StandInError::SharedDomain(base.to_owned()));
        }
        if !is_header_safe(real.expose()) {
            return Err(StandInError::Value);
        }
        Ok(Self {
            origin,
            name: name.to_owned(),
            text: stand_in.to_owned(),
            finder: Finder::new(stand_in).into_owned(),
            real,
            hosts,
            placement: Placement::Header,
        })
    }

    /// The same entry, swapped only in `placement` (header values unless this is called).
    #[must_use]
    pub fn placed(mut self, placement: Placement) -> Self {
        self.placement = placement;
        self
    }

    /// Where it is swapped.
    #[must_use]
    pub fn placement(&self) -> &Placement {
        &self.placement
    }

    /// Whether it is swapped in a header value (or a refresh field) toward `host`.
    fn swaps_in_headers(&self, host: &Host) -> bool {
        self.placement == Placement::Header && self.hosts.contains(host)
    }

    /// What the audit and logs call it: `stand-in:secret:<name>` or `stand-in:login:<name>`.
    #[must_use]
    pub fn id(&self) -> String {
        format!("stand-in:{}:{}", self.origin.label(), self.name)
    }

    /// Where it came from.
    #[must_use]
    pub fn origin(&self) -> StandInOrigin {
        self.origin
    }

    /// The hosts the real value may be swapped in toward.
    #[must_use]
    pub fn hosts(&self) -> &TerminationSet {
        &self.hosts
    }
}

/// A fresh stand-in for the secret `name`, an environment variable's name: `puddle-secret-<name>-`
/// and 128 random bits as 32 hex characters.
///
/// # Errors
/// [`StandInError::Name`] when `name` cannot be one (see [`StandIn::new`]),
/// [`StandInError::Random`] when the operating system has no randomness.
pub fn secret_stand_in(name: &str) -> Result<String, StandInError> {
    let mut random = [0_u8; RANDOM_BYTES];
    getrandom::fill(&mut random).map_err(|_| StandInError::Random)?;
    stand_in_with(name, &random)
}

fn stand_in_with(name: &str, random: &[u8; RANDOM_BYTES]) -> Result<String, StandInError> {
    use std::fmt::Write as _;
    if !is_name(name) {
        return Err(StandInError::Name(name.to_owned()));
    }
    let mut text = format!("puddle-secret-{name}-");
    for byte in random {
        let _ = write!(text, "{byte:02x}");
    }
    Ok(text)
}

/// What a swap did to one request.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Swapped {
    /// The ids of the stand-ins that were replaced (each once).
    pub(crate) swapped: Vec<String>,
    /// The ids of the stand-ins that went out unchanged (each once): seen toward a host they are
    /// not for, or in the request target. Also the ids of stale stand-ins (`stale-stand-in:...`,
    /// see [`stale`]): shaped like one but not in the registry.
    pub(crate) unswapped: Vec<String>,
}

impl Swapped {
    fn note(list: &mut Vec<String>, id: String) {
        if !list.contains(&id) {
            list.push(id);
        }
    }
}

/// One workspace's stand-ins. Cheap to share (`Arc`); safe to change while requests are served.
#[derive(Debug, Default)]
pub struct StandIns {
    entries: RwLock<Vec<Arc<StandIn>>>,
    /// The answer scrubber of each host asked for, made from `entries` as they are; any change to
    /// them empties it.
    scrubbers: Mutex<HashMap<Host, Option<Arc<answer::Scrubber>>>>,
}

impl StandIns {
    /// Empties the answer scrubbers: the entries they were made from change.
    fn forget_scrubbers(&self) {
        self.scrubbers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// An empty registry: nothing is swapped.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `entry`. It applies from the next request.
    ///
    /// # Errors
    /// [`StandInError::Overlaps`] when another entry's stand-in equals this one or one contains
    /// the other.
    pub fn insert(&self, entry: StandIn) -> Result<(), StandInError> {
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        self.forget_scrubbers();
        let overlaps = entries
            .iter()
            .any(|e| e.text.contains(&entry.text) || entry.text.contains(&e.text));
        if overlaps {
            return Err(StandInError::Overlaps);
        }
        entries.push(Arc::new(entry));
        Ok(())
    }

    /// Makes the entries of `origin` exactly `entries`, in one step: a request sees the old
    /// entries or the new ones, never a registry with a stand-in missing. Entries of other origins
    /// stay. An entry that is in both lists (the same stand-in) is replaced, so a new real value
    /// or new hosts apply from the next request.
    ///
    /// # Errors
    /// [`StandInError::Overlaps`] when two stand-ins of the result equal or contain each other;
    /// the registry is not changed then. [`StandInError::Origin`] when an entry is not of `origin`.
    pub fn replace_origin(
        &self,
        origin: StandInOrigin,
        entries: Vec<StandIn>,
    ) -> Result<(), StandInError> {
        if entries.iter().any(|e| e.origin != origin) {
            return Err(StandInError::Origin);
        }
        let mut held = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        self.forget_scrubbers();
        let others: Vec<Arc<StandIn>> = held
            .iter()
            .filter(|e| e.origin != origin)
            .cloned()
            .collect();
        let mut next = others;
        next.extend(entries.into_iter().map(Arc::new));
        let overlap = next.iter().enumerate().any(|(i, a)| {
            next.iter()
                .skip(i + 1)
                .any(|b| a.text.contains(&b.text) || b.text.contains(&a.text))
        });
        if overlap {
            return Err(StandInError::Overlaps);
        }
        *held = next;
        Ok(())
    }

    /// The ids of the entries, `stand-in:secret:<name>` or `stand-in:login:<name>`, in the order
    /// they were registered. Names only: for logs, tests and screens.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|e| e.id())
            .collect()
    }

    /// Removes the entry whose stand-in is `stand_in`. `true` when there was one.
    pub fn remove(&self, stand_in: &str) -> bool {
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        self.forget_scrubbers();
        let before = entries.len();
        entries.retain(|e| e.text != stand_in);
        entries.len() != before
    }

    /// Gives the entry whose stand-in is `stand_in` a new real value (a refreshed token). `true`
    /// when there was one.
    ///
    /// # Errors
    /// [`StandInError::Value`] when the value cannot go in a header.
    pub fn set_real(&self, stand_in: &str, real: SecretValue) -> Result<bool, StandInError> {
        if !is_header_safe(real.expose()) {
            return Err(StandInError::Value);
        }
        let mut entries = self.entries.write().unwrap_or_else(PoisonError::into_inner);
        self.forget_scrubbers();
        let Some(slot) = entries.iter_mut().find(|e| e.text == stand_in) else {
            return Ok(false);
        };
        let old = &**slot;
        *slot = Arc::new(StandIn {
            origin: old.origin,
            name: old.name.clone(),
            text: old.text.clone(),
            finder: old.finder.clone(),
            real,
            hosts: old.hosts.clone(),
            placement: old.placement.clone(),
        });
        Ok(true)
    }

    /// How many stand-ins are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Whether no stand-in is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every host any stand-in is for. A stand-in is swapped only on a terminated connection, so
    /// whoever builds the workspace's [`TerminationSet`] adds these.
    #[must_use]
    pub fn hosts(&self) -> TerminationSet {
        let mut all = TerminationSet::new();
        for entry in self
            .entries
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            all.extend(&entry.hosts);
        }
        all
    }

    /// Swaps the stand-ins in `headers`' values for their real values, for a request to `host`.
    /// Headers `injected` sets are left alone.
    pub(crate) fn swap(
        &self,
        headers: &mut HeaderMap,
        host: &Host,
        injected: Option<&Injection>,
    ) -> Swapped {
        let mut report = Swapped::default();
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let entries: &[Arc<StandIn>] = &guard;
        let skip: Vec<&HeaderName> = injected
            .map(|i| i.headers().iter().map(InjectedHeader::name).collect())
            .unwrap_or_default();
        for (name, value) in headers.iter_mut() {
            if skip.contains(&name) {
                continue;
            }
            // Before the swap, so a real value put in by it is never read.
            for id in stale_in_value(value.as_bytes(), entries) {
                Swapped::note(&mut report.unswapped, id);
            }
            if entries.is_empty() {
                continue;
            }
            if let Some(mut new) = swap_value(value.as_bytes(), entries, host, &mut report) {
                new.set_sensitive(true);
                *value = new;
            }
        }
        report
    }
}

impl StandIns {
    /// The kinds of credential `headers` carry that are neither one of this registry's stand-ins
    /// nor a header `injected` sets: what the workspace's own tools sent (see [`foreign`]).
    /// Read before the swap, so no real value put in by it is looked at.
    pub(crate) fn foreign_credentials(
        &self,
        headers: &HeaderMap,
        injected: Option<&Injection>,
        git_path: bool,
    ) -> Vec<ForeignCredentialKind> {
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        let skip: Vec<&HeaderName> = injected
            .map(|i| i.headers().iter().map(InjectedHeader::name).collect())
            .unwrap_or_default();
        foreign::kinds_in_headers(headers, &skip, git_path, &|value| {
            guard.iter().any(|entry| entry.finder.find(value).is_some())
        })
    }
}

impl StandIns {
    /// Finds the stand-ins in the request `target` (path and query), read as sent and with its
    /// percent-escapes decoded, to report them as gone out unchanged. The registry's own
    /// stand-ins are named, and a value shaped like a stand-in it does not hold is flagged as
    /// stale.
    #[cfg(test)]
    fn scan_target(&self, target: &str) -> Swapped {
        scan(
            &self.entries.read().unwrap_or_else(PoisonError::into_inner),
            target,
        )
    }

    /// Swaps, in the token message `body`, the text of each of the top-level `fields` that is one
    /// of this registry's header stand-ins and is for `host`, for the real value. This is the one
    /// place a body is swapped: only a named field and only an exact match, never a search of the
    /// body. A stand-in in such a field that is not for `host` (or placed elsewhere) is left as it
    /// is and reported, as in a header. `true` in the second place when anything changed.
    pub(crate) fn swap_fields(
        &self,
        body: &mut TokenBody,
        fields: &[String],
        host: &Host,
    ) -> (Swapped, bool) {
        let mut report = Swapped::default();
        let mut changed = false;
        let guard = self.entries.read().unwrap_or_else(PoisonError::into_inner);
        for field in fields {
            let Some(found) = body.text(field).and_then(|text| {
                guard
                    .iter()
                    .find(|entry| entry.text == *text)
                    .map(Arc::clone)
            }) else {
                continue;
            };
            if found.swaps_in_headers(host) {
                changed |= body.set_text(field, found.real.expose());
                Swapped::note(&mut report.swapped, found.id());
            } else {
                Swapped::note(&mut report.unswapped, found.id());
            }
        }
        (report, changed)
    }
}

/// The stand-ins of `entries` in the request `target`, read as sent and with its percent-escapes
/// decoded, as gone out unchanged; a value shaped like a stand-in the registry does not hold is
/// flagged as stale.
fn scan(entries: &[Arc<StandIn>], target: &str) -> Swapped {
    let mut report = Swapped::default();
    // After a swap the target holds real values: the decoded copy is wiped too.
    let decoded = percent_decoded(target.as_bytes()).map(Zeroizing::new);
    for text in std::iter::once(target.as_bytes()).chain(decoded.as_deref().map(Vec::as_slice)) {
        for id in stale::find(text, &|token| holds(entries, token)) {
            Swapped::note(&mut report.unswapped, id);
        }
    }
    for entry in entries {
        let found = entry.finder.find(target.as_bytes()).is_some()
            || decoded
                .as_deref()
                .is_some_and(|decoded| entry.finder.find(decoded).is_some());
        if found {
            Swapped::note(&mut report.unswapped, entry.id());
        }
    }
    report
}

/// Whether the registry holds a stand-in inside `token`.
fn holds(entries: &[Arc<StandIn>], token: &[u8]) -> bool {
    entries
        .iter()
        .any(|entry| entry.finder.find(token).is_some())
}

/// The ids of the stale stand-ins in one header value, read as sent and, for `Basic`, decoded.
fn stale_in_value(value: &[u8], entries: &[Arc<StandIn>]) -> Vec<String> {
    let registered = |token: &[u8]| holds(entries, token);
    let mut ids = stale::find(value, &registered);
    if let Some(decoded) = basic_credentials(value)
        .and_then(|token| LENIENT_BASE64.decode(token).ok())
        .map(Zeroizing::new)
    {
        for id in stale::find(&decoded, &registered) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids
}

/// `input` with its `%XX` escapes decoded, or `None` when it has none.
fn percent_decoded(input: &[u8]) -> Option<Vec<u8>> {
    input.contains(&b'%').then(|| {
        let mut out = Vec::with_capacity(input.len());
        let mut at = 0;
        while let Some(&byte) = input.get(at) {
            let escaped = (byte == b'%')
                .then(|| input.get(at + 1..at + 3))
                .flatten()
                .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
            if let Some(decoded) = escaped {
                out.push(decoded);
                at += 3;
            } else {
                out.push(byte);
                at += 1;
            }
        }
        out
    })
}

/// The new value of one header, or `None` when nothing in it was swapped.
fn swap_value(
    value: &[u8],
    entries: &[Arc<StandIn>],
    host: &Host,
    report: &mut Swapped,
) -> Option<HeaderValue> {
    let real = |entry: &StandIn| {
        entry
            .swaps_in_headers(host)
            .then(|| Zeroizing::new(entry.real.expose().as_bytes().to_vec()))
    };
    if let Some(new) = swap_bytes(value, entries, report, real) {
        return HeaderValue::from_bytes(&new).ok();
    }
    let token = basic_credentials(value)?;
    let decoded = Zeroizing::new(LENIENT_BASE64.decode(token).ok()?);
    let new = swap_bytes(&decoded, entries, report, real)?;
    let scheme = value.get(..5)?;
    let mut out = Zeroizing::new(scheme.to_vec());
    out.push(b' ');
    out.extend_from_slice(STANDARD.encode(&*new).as_bytes());
    HeaderValue::from_bytes(&out).ok()
}

/// The base64 part of an `Authorization`-style `Basic <credentials>` value.
fn basic_credentials(value: &[u8]) -> Option<&[u8]> {
    let scheme = value.get(..5)?;
    if !scheme.eq_ignore_ascii_case(b"basic") {
        return None;
    }
    let rest = value.get(5..)?;
    let credentials = rest.trim_ascii_start();
    let separated = credentials.len() < rest.len();
    (separated && !credentials.is_empty()).then(|| credentials.trim_ascii_end())
}

/// Swaps every stand-in in `input` for which `replacement` gives the bytes to put in its place, in
/// one left-to-right pass, so a replacement is never searched for stand-ins. At each place the
/// longest stand-in wins; one `replacement` refuses is left and reported. `None` when nothing was
/// swapped.
fn swap_bytes(
    input: &[u8],
    entries: &[Arc<StandIn>],
    report: &mut Swapped,
    replacement: impl Fn(&StandIn) -> Option<Zeroizing<Vec<u8>>>,
) -> Option<Zeroizing<Vec<u8>>> {
    // Every place each stand-in occurs, found with one scan per stand-in: the work is bounded by
    // the stand-ins times the value, however many copies a guest puts in it.
    let mut hits: Vec<(usize, &StandIn)> = Vec::new();
    for entry in entries {
        let mut from = 0;
        while let Some(at) = entry.finder.find(input.get(from..)?) {
            hits.push((from + at, entry));
            from += at + entry.text.len();
        }
    }
    hits.sort_by_key(|(at, entry)| (*at, Reverse(entry.text.len())));
    let mut out: Option<Zeroizing<Vec<u8>>> = None;
    let mut copied = 0;
    for (at, entry) in hits {
        if at < copied {
            continue;
        }
        let end = at + entry.text.len();
        if let Some(new) = replacement(entry) {
            let out = out.get_or_insert_with(|| Zeroizing::new(Vec::with_capacity(input.len())));
            out.extend_from_slice(input.get(copied..at)?);
            out.extend_from_slice(&new);
            copied = end;
            Swapped::note(&mut report.swapped, entry.id());
        } else {
            Swapped::note(&mut report.unswapped, entry.id());
        }
    }
    let mut out = out?;
    out.extend_from_slice(input.get(copied..)?);
    Some(out)
}

#[cfg(test)]
mod tests;
