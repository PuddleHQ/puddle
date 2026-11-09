// SPDX-License-Identifier: GPL-3.0-or-later
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
use puddle_types::{Event, EventSink, ForeignCredentialKind, Host, WorkspaceId};
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
}

impl ForeignCredentialWarning {
    /// A warning that is on while `enabled` says so.
    #[must_use]
    pub fn new(enabled: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        Self {
            enabled: Arc::new(enabled),
            told: Mutex::new(HashSet::new()),
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
            let first = {
                let mut told = self.told.lock().unwrap_or_else(PoisonError::into_inner);
                told.len() < MAX_SEEN && told.insert((host.to_string(), *kind))
            };
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
    if GITHUB_PREFIXES
        .iter()
        .any(|prefix| has_checksummed_body(token, prefix))
    {
        return true;
    }
    token.strip_prefix(FINE_GRAINED_PREFIX).is_some_and(|body| {
        body.len() == FINE_GRAINED_ID + 1 + FINE_GRAINED_SECRET
            && body.get(FINE_GRAINED_ID) == Some(&b'_')
            && is_base62(body.get(..FINE_GRAINED_ID).unwrap_or_default())
            && is_base62(body.get(FINE_GRAINED_ID + 1..).unwrap_or_default())
    })
}

fn is_npm(token: &[u8]) -> bool {
    has_checksummed_body(token, NPM_PREFIX)
}

fn is_gitlab(token: &[u8]) -> bool {
    token.strip_prefix(GITLAB_PREFIX).is_some_and(|body| {
        (GITLAB_MIN_BODY..=GITLAB_MAX_BODY).contains(&body.len())
            && body
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    })
}

fn is_azure_devops(token: &[u8]) -> bool {
    token.len() == AZURE_LEN
        && is_base62(token)
        && token.get(AZURE_MARK_AT..AZURE_MARK_AT + AZURE_MARK.len()) == Some(AZURE_MARK)
}

#[cfg(test)]
mod tests {
    use ::http::header::{AUTHORIZATION, HeaderName, HeaderValue};

    use super::*;

    /// A made-up 36-character body whose last six characters are the checksum of the first
    /// thirty, built with the same function the check uses.
    fn checksummed(prefix: &str, entropy: &str) -> String {
        assert_eq!(entropy.len(), 30);
        let tail = stale::checksum_of(entropy.as_bytes());
        format!("{prefix}{entropy}{}", std::str::from_utf8(&tail).unwrap())
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn kinds(pairs: &[(&str, &str)], git: bool) -> Vec<ForeignCredentialKind> {
        kinds_in_headers(&headers(pairs), &[], git, &|_| false)
    }

    const NONE: [ForeignCredentialKind; 0] = [];

    const ENTROPY: &str = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5";

    #[test]
    fn a_github_token_with_a_valid_checksum_is_named_on_any_path() {
        for prefix in ["ghp_", "gho_", "ghu_", "ghs_", "ghr_"] {
            let token = checksummed(prefix, ENTROPY);
            for git in [false, true] {
                assert_eq!(
                    kinds(&[("authorization", &format!("Bearer {token}"))], git),
                    [ForeignCredentialKind::GithubToken],
                    "{prefix} git={git}"
                );
            }
        }
    }

    #[test]
    fn a_github_token_with_a_wrong_checksum_is_not_named_here() {
        let token = format!("ghp_{ENTROPY}AAAAAA");
        assert_eq!(
            kinds(&[("authorization", &format!("token {token}"))], false),
            NONE
        );
    }

    #[test]
    fn a_fine_grained_github_token_is_read_by_its_shape() {
        let token = format!("github_pat_{}_{}", "A1b2C3d4E5f6G7h8I9j0K1", "x".repeat(59));
        assert_eq!(
            kinds(&[("authorization", &format!("Bearer {token}"))], false),
            [ForeignCredentialKind::GithubToken]
        );
        let short = format!("github_pat_{}_{}", "A1b2C3d4E5f6G7h8I9j0K1", "x".repeat(58));
        assert_eq!(
            kinds(&[("authorization", &format!("Bearer {short}"))], false),
            NONE
        );
    }

    #[test]
    fn an_npm_token_with_a_valid_checksum_is_named() {
        let token = checksummed("npm_", ENTROPY);
        assert_eq!(
            kinds(&[("authorization", &format!("Bearer {token}"))], false),
            [ForeignCredentialKind::NpmToken]
        );
        let broken = format!("npm_{ENTROPY}000000");
        assert_eq!(
            kinds(&[("authorization", &format!("Bearer {broken}"))], false),
            NONE
        );
    }

    #[test]
    fn a_gitlab_token_is_read_by_prefix_and_shape_including_the_private_token_header() {
        let token = "glpat-AbCdEfGhIjKlMnOpQrSt";
        assert_eq!(
            kinds(&[("authorization", &format!("Bearer {token}"))], false),
            [ForeignCredentialKind::GitlabToken]
        );
        assert_eq!(
            kinds(&[("private-token", token)], false),
            [ForeignCredentialKind::GitlabToken]
        );
        assert_eq!(
            kinds(&[("authorization", "Bearer glpat-short")], false),
            NONE
        );
    }

    #[test]
    fn a_newer_azure_devops_token_is_named_inside_basic_credentials() {
        let mut token = "B".repeat(AZURE_LEN);
        token.replace_range(AZURE_MARK_AT..AZURE_MARK_AT + 4, "AZDO");
        let basic = format!("Basic {}", STANDARD.encode(format!(":{token}")));
        assert_eq!(
            kinds(&[("authorization", &basic)], false),
            [ForeignCredentialKind::AzureDevopsToken]
        );
        let mut unmarked = token.clone();
        unmarked.replace_range(AZURE_MARK_AT..AZURE_MARK_AT + 4, "ABCD");
        let basic = format!("Basic {}", STANDARD.encode(format!(":{unmarked}")));
        assert_eq!(kinds(&[("authorization", &basic)], false), NONE);
    }

    #[test]
    fn a_known_token_as_the_password_of_basic_credentials_is_named() {
        let token = checksummed("ghp_", ENTROPY);
        let basic = format!(
            "Basic {}",
            STANDARD.encode(format!("x-access-token:{token}"))
        );
        assert_eq!(
            kinds(&[("authorization", &basic)], true),
            [ForeignCredentialKind::GithubToken]
        );
    }

    #[test]
    fn a_token_after_basic_that_is_not_base64_is_read_as_it_stands() {
        let token = checksummed("ghp_", ENTROPY);
        assert_eq!(
            kinds(&[("authorization", &format!("Basic {token}"))], false),
            [ForeignCredentialKind::GithubToken]
        );
    }

    #[test]
    fn any_authorization_on_a_git_path_counts_and_off_a_git_path_it_stays_silent() {
        let value = "Bearer an-opaque-value-of-no-known-kind";
        assert_eq!(
            kinds(&[("authorization", value)], true),
            [ForeignCredentialKind::GitAuthorization]
        );
        assert_eq!(kinds(&[("authorization", value)], false), NONE);
        // Another header never counts as an Authorization.
        assert_eq!(kinds(&[("x-custom", value)], true), NONE);
        // Nothing to name when the value is empty.
        assert_eq!(kinds(&[("authorization", "")], true), NONE);
    }

    #[test]
    fn a_stale_stand_in_is_left_to_its_own_check_and_not_counted_as_an_authorization() {
        let stale_one = "puddle-secret-API_KEY-00112233445566778899aabbccddeeff";
        assert_eq!(
            kinds(&[("authorization", &format!("Bearer {stale_one}"))], true),
            NONE
        );
        let wrong_sum = format!("gho_{ENTROPY}AAAAAA");
        assert_eq!(
            kinds(&[("authorization", &format!("token {wrong_sum}"))], true),
            NONE
        );
    }

    #[test]
    fn a_header_that_holds_one_of_the_workspaces_stand_ins_is_not_looked_at() {
        let token = checksummed("ghp_", ENTROPY);
        let map = headers(&[("authorization", &format!("Bearer {token}"))]);
        let held = kinds_in_headers(&map, &[], true, &|_| true);
        assert_eq!(held, NONE);
    }

    #[test]
    fn a_header_puddle_injected_is_not_looked_at() {
        let token = checksummed("ghp_", ENTROPY);
        let map = headers(&[("authorization", &format!("Bearer {token}"))]);
        let skip = [&AUTHORIZATION];
        assert_eq!(kinds_in_headers(&map, &skip, true, &|_| false), NONE);
    }

    #[test]
    fn each_kind_is_named_once_and_in_header_order() {
        let github = checksummed("ghp_", ENTROPY);
        let npm = checksummed("npm_", ENTROPY);
        let map = headers(&[
            ("authorization", &format!("Bearer {github}")),
            ("authorization", &format!("Bearer {github}")),
            ("private-token", "glpat-AbCdEfGhIjKlMnOpQrSt"),
            ("authorization", &format!("Bearer {npm}")),
        ]);
        assert_eq!(
            kinds_in_headers(&map, &[], false, &|_| false),
            [
                ForeignCredentialKind::GithubToken,
                ForeignCredentialKind::NpmToken,
                ForeignCredentialKind::GitlabToken,
            ]
        );
    }

    #[test]
    fn a_value_over_the_limit_is_not_read() {
        let long = format!("Bearer {}", "a".repeat(MAX_VALUE));
        assert_eq!(kinds(&[("authorization", &long)], true), NONE);
    }
}
