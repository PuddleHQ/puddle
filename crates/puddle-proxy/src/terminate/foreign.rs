// SPDX-License-Identifier: AGPL-3.0-or-later
//! Credentials in a request that Puddle did not add or swap.
//!
//! A tool in the workspace may hold a token of its own (one the user pasted, a login Puddle does
//! not capture). Puddle lets it through as it always did and, unless the user turned the warning
//! off, says so: the user then sees that a credential reaches a host without Puddle in the loop.
//! What the bytes alone can tell apart:
//!
//! - a token of a known kind whose checksum is valid: GitHub (`ghp_`, `gho_`, `ghu_`, `ghs_`,
//!   `ghr_`, and the fine-grained `github_pat_`), npm (`npm_`), GitLab (`glpat-`) and the newer,
//!   84-character Azure DevOps token (`AZDO` at 76 to 79). GitHub and npm tokens end in a CRC-32
//!   of the rest, which is checked; the other kinds are read by prefix and shape, because their
//!   checksums are not published as a specification (the fine-grained GitHub token's included);
//! - on a request to a Git repository, any `Authorization` header (an old Azure DevOps token and
//!   a GitHub token from before 2021 carry no marker). Off a Git path an `Authorization` of
//!   unknown shape stays silent: it is too often harmless (a registry's anonymous token, a local
//!   service's own session).
//!
//! A stale stand-in of Puddle's own is found by [`super::stale`] and joins the warning in
//! `Outcome::note_stand_ins`.
//!
//! Puddle cannot tell the user's own token from a leaked one, so this catches mistakes, not
//! attackers: a hostile workspace can send a token to a host that is not decrypted. Nothing here
//! keeps, logs or returns a value: the result is only the kind.

use std::collections::HashSet;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use ::http::header::HeaderMap;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use puddle_types::{Event, EventSink, ForeignCredentialKind, Host, UnknownTokenKind, WorkspaceId};
use zeroize::Zeroizing;

use super::stale;

/// Most (host, kind) pairs remembered, so a workspace that sends to many hosts cannot grow the
/// list without bound.
const MAX_SEEN: usize = 256;

/// Whether a workspace wants the warning, and which findings it was already told about.
///
/// The question is asked each time a credential is found (never otherwise), so a change to the
/// setting applies to the next request. The notice is raised once for each host and kind while
/// the workspace runs; the activity record of every connection that carried one is flagged.
pub struct ForeignCredentialWarning {
    enabled: Arc<dyn Fn() -> bool + Send + Sync>,
    told: Mutex<HashSet<(String, ForeignCredentialKind)>>,
    told_answers: Mutex<HashSet<(String, UnknownTokenKind)>>,
}

impl ForeignCredentialWarning {
    /// A warning that is on while `enabled` says so.
    #[must_use]
    pub fn new(enabled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            enabled: Arc::new(enabled),
            told: Mutex::new(HashSet::new()),
            told_answers: Mutex::new(HashSet::new()),
        }
    }

    /// Whether the warning is on now.
    #[must_use]
    pub fn is_on(&self) -> bool {
        (self.enabled)()
    }

    /// Notes that a request to `host` carried credentials of `kinds`: each joins `flagged` (the
    /// connection record's list) once, and the first sight of a host and kind raises the notice.
    /// Nothing when the warning is off. Only the host and the kinds are read, logged or sent.
    pub(crate) fn note(
        &self,
        workspace: &WorkspaceId,
        host: &Host,
        sink: &dyn EventSink,
        flagged: &mut Vec<String>,
        kinds: &[ForeignCredentialKind],
    ) {
        if kinds.is_empty() || !self.is_on() {
            return;
        }
        for kind in kinds {
            let name = kind.as_str();
            if !flagged.iter().any(|seen| seen == name) {
                flagged.push(name.to_owned());
            }
            let first = first_sight(&self.told, host, *kind);
            if first {
                tracing::info!(%workspace, %host, kind = name, "a request carried a credential Puddle did not add");
                sink.emit(Event::ForeignCredential {
                    workspace: workspace.clone(),
                    host: host.to_string(),
                    kind: *kind,
                });
            }
        }
    }
}

impl ForeignCredentialWarning {
    /// Notes that an answer from `host` held a token of `kind` that Puddle does not hold: the
    /// kind joins `flagged` (the connection record's list) once, and the first sight of a host
    /// and kind raises the notice. Nothing when the warning is off. Only the host and the kind
    /// are read, logged or sent. The same setting turns this and the warning about requests off.
    pub(crate) fn note_answer(
        &self,
        workspace: &WorkspaceId,
        host: &Host,
        sink: &dyn EventSink,
        flagged: &mut Vec<String>,
        kind: UnknownTokenKind,
    ) {
        if !self.is_on() {
            return;
        }
        let name = kind.as_str();
        if !flagged.iter().any(|seen| seen == name) {
            flagged.push(name.to_owned());
        }
        if first_sight(&self.told_answers, host, kind) {
            tracing::info!(%workspace, %host, kind = name, "an answer held a token Puddle does not hold");
            sink.emit(Event::UnknownTokenInAnswer {
                workspace: workspace.clone(),
                host: host.to_string(),
                kind,
            });
        }
    }
}

/// Whether this is the first time `host` and `kind` are seen, remembering them (up to
/// [`MAX_SEEN`] pairs: past that nothing is new, so the list cannot grow without bound).
fn first_sight<K: std::hash::Hash + Eq>(
    told: &Mutex<HashSet<(String, K)>>,
    host: &Host,
    kind: K,
) -> bool {
    let mut told = told.lock().unwrap_or_else(PoisonError::into_inner);
    told.len() < MAX_SEEN && told.insert((host.to_string(), kind))
}

impl Default for ForeignCredentialWarning {
    /// On, as the setting is by default.
    fn default() -> Self {
        Self::new(|| true)
    }
}

impl fmt::Debug for ForeignCredentialWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ForeignCredentialWarning")
            .finish_non_exhaustive()
    }
}

/// The longest header value read: a credential is short, and a longer one is not decoded.
const MAX_VALUE: usize = 8192;

const BASE62_LEN: usize = 36;
const GITHUB_PREFIXES: [&[u8]; 5] = [b"ghp_", b"gho_", b"ghu_", b"ghs_", b"ghr_"];
const FINE_GRAINED_PREFIX: &[u8] = b"github_pat_";
const FINE_GRAINED_ID: usize = 22;
const FINE_GRAINED_SECRET: usize = 59;
const NPM_PREFIX: &[u8] = b"npm_";
const GITLAB_PREFIX: &[u8] = b"glpat-";
const GITLAB_MIN_BODY: usize = 20;
const GITLAB_MAX_BODY: usize = 300;
const AZURE_LEN: usize = 84;
const AZURE_MARK: &[u8] = b"AZDO";
const AZURE_MARK_AT: usize = 76;

/// Headers that carry a credential a tool set itself, besides `Authorization`.
const OTHER_HEADERS: [&str; 1] = ["private-token"];

/// What kinds of credential `headers` carry, each once, in header order. `holds_stand_in` says
/// whether a header value holds one of the workspace's stand-ins (Puddle's own credential, which
/// the swap handles: such a header is not looked at). `skip` names the headers Puddle injected.
/// `git_path`: the request is a Git request, where any `Authorization` counts.
pub(crate) fn kinds_in_headers(
    headers: &HeaderMap,
    skip: &[&::http::header::HeaderName],
    git_path: bool,
    holds_stand_in: &dyn Fn(&[u8]) -> bool,
) -> Vec<ForeignCredentialKind> {
    let mut kinds: Vec<ForeignCredentialKind> = Vec::new();
    for (name, value) in headers {
        let is_authorization = name == ::http::header::AUTHORIZATION;
        if !is_authorization && !OTHER_HEADERS.contains(&name.as_str()) {
            continue;
        }
        let bytes = value.as_bytes();
        if skip.contains(&name) || bytes.len() > MAX_VALUE || holds_stand_in(bytes) {
            continue;
        }
        for kind in kinds_in_value(bytes, git_path && is_authorization) {
            if !kinds.contains(&kind) {
                kinds.push(kind);
            }
        }
    }
    kinds
}

/// The kinds one credential header value carries. `git_path_authorization`: an `Authorization` of
/// a Git request, where a value of no known kind still counts.
fn kinds_in_value(value: &[u8], git_path_authorization: bool) -> Vec<ForeignCredentialKind> {
    let parts = parts_of(value);
    let mut kinds: Vec<ForeignCredentialKind> = Vec::new();
    for part in &parts {
        if let Some(kind) = kind_of_token(part)
            && !kinds.contains(&kind)
        {
            kinds.push(kind);
        }
    }
    if kinds.is_empty()
        && git_path_authorization
        && !parts.iter().all(|part| part.is_empty())
        && parts
            .iter()
            .all(|part| stale::find(part, &|_| false).is_empty())
    {
        kinds.push(ForeignCredentialKind::GitAuthorization);
    }
    kinds
}

/// The pieces of a credential header value that may be a token: the text after the scheme word
/// (`Bearer`, `token`, `Basic`), or for `Basic` the user name and the password of what its
/// base64 holds.
fn parts_of(value: &[u8]) -> Vec<Zeroizing<Vec<u8>>> {
    let value = value.trim_ascii();
    let (scheme, rest) = match value.iter().position(u8::is_ascii_whitespace) {
        Some(at) => (
            value.get(..at).unwrap_or_default(),
            value.get(at..).unwrap_or_default().trim_ascii(),
        ),
        None => (&[][..], value),
    };
    let own = |bytes: &[u8]| Zeroizing::new(bytes.to_vec());
    if scheme.eq_ignore_ascii_case(b"basic") {
        let Ok(decoded) = STANDARD.decode(rest).map(Zeroizing::new) else {
            return vec![own(rest)];
        };
        return decoded.split(|b| *b == b':').map(own).collect();
    }
    if scheme.is_empty() {
        return vec![own(value)];
    }
    vec![own(rest)]
}

/// The kind of a whole token, when its bytes say.
fn kind_of_token(token: &[u8]) -> Option<ForeignCredentialKind> {
    if is_github(token) {
        Some(ForeignCredentialKind::GithubToken)
    } else if is_npm(token) {
        Some(ForeignCredentialKind::NpmToken)
    } else if is_gitlab(token) {
        Some(ForeignCredentialKind::GitlabToken)
    } else if is_azure_devops(token) {
        Some(ForeignCredentialKind::AzureDevopsToken)
    } else {
        None
    }
}

fn is_base62(bytes: &[u8]) -> bool {
    bytes.iter().all(u8::is_ascii_alphanumeric)
}

/// A token of a prefix, then 36 base62 characters ending in the checksum of the first thirty.
fn has_checksummed_body(token: &[u8], prefix: &[u8]) -> bool {
    token.strip_prefix(prefix).is_some_and(|body| {
        body.len() == BASE62_LEN && is_base62(body) && stale::has_valid_checksum(body)
    })
}

fn is_github(token: &[u8]) -> bool {
    is_github_checksummed(token) || is_fine_grained(token)
}

/// A classic GitHub token: a prefix, then 36 base62 characters ending in their checksum.
pub(super) fn is_github_checksummed(token: &[u8]) -> bool {
    GITHUB_PREFIXES
        .iter()
        .any(|prefix| has_checksummed_body(token, prefix))
}

/// A fine-grained GitHub token, read by its shape.
pub(super) fn is_fine_grained(token: &[u8]) -> bool {
    token.strip_prefix(FINE_GRAINED_PREFIX).is_some_and(|body| {
        body.len() == FINE_GRAINED_ID + 1 + FINE_GRAINED_SECRET
            && body.get(FINE_GRAINED_ID) == Some(&b'_')
            && is_base62(body.get(..FINE_GRAINED_ID).unwrap_or_default())
            && is_base62(body.get(FINE_GRAINED_ID + 1..).unwrap_or_default())
    })
}

pub(super) fn is_npm(token: &[u8]) -> bool {
    has_checksummed_body(token, NPM_PREFIX)
}

pub(super) fn is_gitlab(token: &[u8]) -> bool {
    token.strip_prefix(GITLAB_PREFIX).is_some_and(|body| {
        (GITLAB_MIN_BODY..=GITLAB_MAX_BODY).contains(&body.len())
            && body
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    })
}

pub(super) fn is_azure_devops(token: &[u8]) -> bool {
    token.len() == AZURE_LEN
        && is_base62(token)
        && token.get(AZURE_MARK_AT..AZURE_MARK_AT + AZURE_MARK.len()) == Some(AZURE_MARK)
}

#[cfg(test)]
mod tests;
