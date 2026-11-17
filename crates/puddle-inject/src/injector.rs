// SPDX-License-Identifier: AGPL-3.0-or-later
//! The injector for one workspace: what happens to each request on a decrypted Git host.
//!
//! In order, for a request that reads as a Git request (anything else passes through untouched):
//!
//! 1. A path that names a repository in more than one way is refused (`400`).
//! 2. The workspace's repository table is applied: a push to a repository without Push, or, when
//!    the workspace asks for it, a fetch from one without Pull, is refused (`403`) with a one-line
//!    message and an event the UI turns into an "add it" notice. This holds whoever supplies the
//!    credential.
//! 3. A request that carries its own `Authorization` goes out as it is: never replaced, never
//!    dropped, and a `401` to it is the server's answer.
//! 4. Otherwise the identity that covers the repository's owner supplies the credential. No
//!    covering identity: the request goes out without one and the server's `401` is its answer (an
//!    event tells the user no identity covers the owner, once the workspace's own tool has had a
//!    moment to retry with a credential it holds). A
//!    source that cannot supply the secret: `502` (the cache the secret is read through raises the
//!    sign-in notice), except for a read (clone, fetch, LFS download): that goes out with no
//!    credential, and the server's `401` to it becomes the same `502`. A push and every write stay refused. A `401` to the credential Puddle added becomes a `403` and a sign-in notice:
//!    the workspace holds no credential to answer it with.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_proxy::{
    BoxFuture, InjectContext, InjectDecision, InjectRefusal, InjectedHeader, Injection, Injector,
    RequestView, SecretValue, Unauthorized,
};
use puddle_secrets::{SourceError, SourceSpec};
use puddle_store::{CredentialBinding, CredentialChoice, Identity, WorkspaceGit};
use puddle_types::{Event, EventSink, GitAccess, ReasonCode, WorkspaceId};
use tokio::time::Instant;

use crate::git_path::{Access, Classified, GitPath, classify};
use crate::header::authorization;
use crate::source::{CredentialSource, GitSettings};

/// The most request body read to tell an LFS download from an upload. A batch of a hundred
/// objects is about 10 KiB.
const MAX_LFS_BATCH: usize = 256 * 1024;

/// What the guest reads when the store offers a way to choose that this reading does not know.
const UNCHOOSABLE: &str = "Puddle could not choose a credential for this request";

/// A notice about the same thing is raised at most once in this long, however many requests
/// meet it (a Git client retries).
const NOTICE_EVERY: Duration = Duration::from_secs(10);

/// Most notices remembered, per kind.
const MAX_NOTICES: usize = 256;

/// After the server answers a request that no identity covers with `401`, the workspace's own
/// tool gets this long to retry with a credential it holds (a credential helper, `.netrc`, the
/// address's user name and password) before the user is told no identity covers the owner.
const NO_IDENTITY_GRACE: Duration = Duration::from_secs(3);

/// The injector of one workspace.
pub struct GitInjector {
    workspace: WorkspaceId,
    settings: Arc<dyn GitSettings>,
    credentials: Arc<dyn CredentialSource>,
    events: Arc<dyn EventSink>,
    /// Refused pushes and pulls, by repository: the guest names those at will, so they have a
    /// table of their own, and a flood of them cannot silence the other notices.
    denied: Arc<Notices>,
    /// "No identity covers it", by host.
    no_identity: Arc<Notices>,
    /// "Sign in again", by source: one of the user's own, never a name the guest picks.
    sign_in: Arc<Notices>,
    /// When a request of the workspace's own carrying an `Authorization` last went to each owner.
    authed: Arc<Notices>,
}

impl fmt::Debug for GitInjector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GitInjector")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl GitInjector {
    /// The injector of `workspace`, reading its Git `settings` and the secrets its identities name
    /// from `credentials`, and raising notices on `events`.
    #[must_use]
    pub fn new(
        workspace: WorkspaceId,
        settings: Arc<dyn GitSettings>,
        credentials: Arc<dyn CredentialSource>,
        events: Arc<dyn EventSink>,
    ) -> Self {
        Self {
            workspace,
            settings,
            credentials,
            events,
            denied: Arc::new(Notices::default()),
            no_identity: Arc::new(Notices::default()),
            sign_in: Arc::new(Notices::default()),
            authed: Arc::new(Notices::default()),
        }
    }

    async fn decide_git(
        &self,
        context: &InjectContext<'_>,
        request: &RequestView<'_>,
    ) -> InjectDecision {
        let host = context.host.to_string();
        let path = match classify(&host, request.method(), request.path(), request.query()) {
            Classified::NotGit => return InjectDecision::PassThrough,
            Classified::Ambiguous(why) => {
                return refuse(
                    400,
                    ReasonCode::BadGitPath,
                    format!(
                        "this request names a Git repository in a way Puddle cannot read safely ({why}); nothing was sent to {host}. Use the repository's plain address"
                    ),
                );
            }
            Classified::Git(path) => path,
        };
        let git = match self.settings.current().await {
            Ok(git) => git,
            Err(err) => {
                tracing::warn!(workspace = %self.workspace, error = %err, "the Git settings could not be read");
                return refuse(
                    502,
                    ReasonCode::GitSettingsUnavailable,
                    format!(
                        "Puddle could not read this workspace's Git settings ({err}); nothing was sent to {host}"
                    ),
                );
            }
        };
        let access = match path.access {
            Access::Pull => GitAccess::Pull,
            Access::Push => GitAccess::Push,
            Access::Batch => batch_access(request.body()),
        };
        if let Some(refusal) = self.check_table(&git, &path, access) {
            return InjectDecision::Refuse(refusal);
        }
        if request.header("authorization").is_some() {
            self.authed.mark(owner_key(&path));
            return InjectDecision::PassThroughOwnCredential;
        }
        match covering(&git, &path) {
            CredentialChoice::Covered {
                identity, binding, ..
            } => self.inject(identity, binding, &path, access).await,
            // No identity covers it: it goes out as the workspace sent it, so a token the workspace
            // supplies after the server's `401` (a credential helper, `.netrc`, the address's user
            // name and password) is used as it would be without Puddle.
            CredentialChoice::Uncovered => {
                tracing::info!(workspace = %self.workspace, host = %path.host, owner = %path.owner, "no identity covers this owner; the request goes out without a credential");
                // A private repository answers `401`; if the tool then has no credential either,
                // the user is told why (a public one never does).
                InjectDecision::PassThroughNoting(self.unanswered(&path))
            }
            CredentialChoice::Ambiguous(ids) => {
                let labels = git
                    .identities
                    .iter()
                    .filter(|identity| ids.contains(&identity.id))
                    .map(|identity| identity.label.as_str())
                    .collect::<Vec<_>>()
                    .join(" and ");
                refuse(
                    409,
                    ReasonCode::CredentialAmbiguous,
                    format!(
                        "{labels} both cover {}/{}; narrow one in Puddle",
                        path.host, path.owner
                    ),
                )
            }
            // A choice a later Puddle adds: nothing it says is trusted.
            _ => refuse(502, ReasonCode::CredentialUnavailable, UNCHOOSABLE),
        }
    }

    /// The refusal for a push or fetch the workspace's table does not allow, with the event that
    /// lets the user add the repository.
    fn check_table(
        &self,
        git: &WorkspaceGit,
        path: &GitPath,
        access: GitAccess,
    ) -> Option<InjectRefusal> {
        let repo = path.repo_ref();
        let allowed = match (access, &repo) {
            (GitAccess::Push, Some(repo)) => git.allows_push(repo),
            (GitAccess::Pull, Some(repo)) => git.allows_pull(repo),
            (GitAccess::Push, None) => !git.only_push_listed,
            (GitAccess::Pull, None) => !git.only_pull_listed,
        };
        if allowed {
            return None;
        }
        let (code, verb, list, switch) = match access {
            GitAccess::Push => (
                ReasonCode::PushDenied,
                "push to",
                "push",
                "Only push to listed repos",
            ),
            GitAccess::Pull => (
                ReasonCode::PullDenied,
                "pull from",
                "pull",
                "Only pull from listed repos",
            ),
        };
        let what = path.describe();
        let message = match &repo {
            Some(repo) => {
                if self.denied.first(format!("{code}:{repo}")) {
                    self.events.emit(Event::GitAccessDenied {
                        workspace: self.workspace.clone(),
                        host: repo.host.to_string(),
                        owner: repo.owner.to_string(),
                        repo: repo.repo.clone(),
                        access,
                    });
                }
                let name = &repo.repo;
                format!(
                    "{verb} {what} is not on this workspace's {list} list yet, as with a repository the workspace just created or forked; click \"Allow {list} for {name}\" in the notice (it works at once, no restart), or turn off \"{switch}\" on the workspace's Git tab, then try again"
                )
            }
            None => format!(
                "{verb} {what} is not allowed: the repository list cannot hold this name, so turn off \"{switch}\" on the workspace's Git tab"
            ),
        };
        tracing::info!(workspace = %self.workspace, %code, repo = %what, "refused by the repository list");
        Some(InjectRefusal::new(403, code, message))
    }

    /// Adds `identity`'s credential for `path`.
    async fn inject(
        &self,
        identity: &Identity,
        binding: &CredentialBinding,
        path: &GitPath,
        access: GitAccess,
    ) -> InjectDecision {
        let source = &binding.source;
        let named = format!("{}/{}", path.host, path.owner);
        // The token is checked against where it is about to go, as its source asks.
        let scope = source.scope();
        let right_place = scope
            .host
            .as_str()
            .eq_ignore_ascii_case(binding.host.as_str())
            && scope
                .org
                .as_ref()
                .is_none_or(|org| org.as_str().eq_ignore_ascii_case(&path.owner));
        if !right_place {
            tracing::warn!(workspace = %self.workspace, identity = %identity.label, "a stored credential is not for the place it covers");
            return refuse(
                502,
                ReasonCode::CredentialUnavailable,
                format!(
                    "the credential of identity {} is not for {named}; fix the identity in Puddle",
                    identity.label
                ),
            );
        }
        let credential = match self.credentials.credential(source).await {
            Ok(credential) => credential,
            Err(err) => {
                tracing::info!(workspace = %self.workspace, source = %source.describe(), "the credential could not be read: {err}");
                // The cache the source is read through says when a sign-in is needed (and the host
                // turns that into a notice), so it is not said twice here.
                let advice = if err.needs_sign_in() {
                    "sign in to it in Puddle"
                } else {
                    "check it in Puddle"
                };
                if access == GitAccess::Pull {
                    return self.anonymous_read(identity, source, path, &err, advice);
                }
                return refuse(
                    502,
                    ReasonCode::CredentialUnavailable,
                    format!(
                        "the credential of identity {} for {named} ({}) is not available: {err}; {advice}",
                        identity.label,
                        source.describe()
                    ),
                );
            }
        };
        let value = authorization(&path.credential_host, credential.secret.expose());
        let rejected = self.rejected(identity, source, path);
        let binding_id = format!("identity:{}:{}", identity.id, binding.host);
        credentialed(value, binding_id, &identity.label, &named, rejected)
    }

    /// A read (clone, fetch, an LFS download) of a repository whose identity cannot supply its secret (not signed in, tool missing) goes
    /// out with no credential at all, so a public repository still answers. A private one answers
    /// `401`, which becomes the refusal a missing credential would have been, never the
    /// server's `401` (the workspace's tool would ask for a password that does not exist).
    /// The notice was raised by the cache the secret is read through.
    fn anonymous_read(
        &self,
        identity: &Identity,
        source: &SourceSpec,
        path: &GitPath,
        err: &SourceError,
        advice: &str,
    ) -> InjectDecision {
        tracing::info!(workspace = %self.workspace, identity = %identity.label, "identity not signed in; the read goes out without a credential");
        let message = format!(
            "{} needs a credential for {}/{} and went without one: the credential of identity {} ({}) is not available: {err}; {advice}",
            path.credential_host,
            path.host,
            path.owner,
            identity.label,
            source.describe()
        );
        InjectDecision::PassThroughNoting(Unauthorized::new(move || {
            InjectRefusal::new(502, ReasonCode::CredentialUnavailable, message.clone())
        }))
    }

    /// What a `401` to a request no identity covers does: after [`NO_IDENTITY_GRACE`] without the
    /// workspace's own tool retrying with a credential, an event says no identity covers the
    /// owner. The server's answer reaches the workspace as it is.
    fn unanswered(&self, path: &GitPath) -> Unauthorized {
        let events = Arc::clone(&self.events);
        let notices = Arc::clone(&self.no_identity);
        let authed = Arc::clone(&self.authed);
        let workspace = self.workspace.clone();
        let (host, owner, repo) = (path.host.clone(), path.owner.clone(), path.repo.clone());
        let key = owner_key(path);
        Unauthorized::noting(move || {
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return;
            };
            let asked = Instant::now();
            let (events, notices, authed) = (
                Arc::clone(&events),
                Arc::clone(&notices),
                Arc::clone(&authed),
            );
            let (workspace, host, owner, repo, key) = (
                workspace.clone(),
                host.clone(),
                owner.clone(),
                repo.clone(),
                key.clone(),
            );
            runtime.spawn(async move {
                tokio::time::sleep(NO_IDENTITY_GRACE).await;
                if !authed.marked_since(&key, asked) && notices.first(host.clone()) {
                    events.emit(Event::GitNoIdentity {
                        workspace,
                        host,
                        owner,
                        repo,
                    });
                }
            });
        })
    }

    /// What a `401` to the credential Puddle added becomes.
    fn rejected(&self, identity: &Identity, source: &SourceSpec, path: &GitPath) -> Unauthorized {
        let credentials = Arc::clone(&self.credentials);
        let events = Arc::clone(&self.events);
        let notices = Arc::clone(&self.sign_in);
        let (source, label) = (source.clone(), identity.label.clone());
        let (host, named) = (
            path.credential_host.clone(),
            format!("{}/{}", path.host, path.owner),
        );
        Unauthorized::new(move || {
            // The user may have signed in again since the token was read.
            credentials.forget(&source);
            if notices.first(source.describe()) {
                events.emit(Event::CredentialSignInNeeded {
                    host: host.clone(),
                    source: source.describe(),
                });
            }
            InjectRefusal::new(
                403,
                ReasonCode::CredentialRejected,
                format!(
                    "{host} did not accept the credential of identity {label} ({}) for {named}; sign in again or check that the token can reach it, in Puddle",
                    source.describe()
                ),
            )
        })
    }
}

impl Injector for GitInjector {
    fn decide<'a>(
        &'a self,
        context: &'a InjectContext<'a>,
        request: &'a RequestView<'a>,
    ) -> BoxFuture<'a, InjectDecision> {
        Box::pin(self.decide_git(context, request))
    }

    fn body_wanted(&self, context: &InjectContext<'_>, request: &RequestView<'_>) -> Option<usize> {
        let host = context.host.to_string();
        match classify(&host, request.method(), request.path(), request.query()) {
            Classified::Git(GitPath {
                access: Access::Batch,
                ..
            }) => Some(MAX_LFS_BATCH),
            Classified::NotGit | Classified::Ambiguous(_) | Classified::Git(_) => None,
        }
    }
}

/// The decision that sends `value` as the `Authorization` header, or the refusal that says the
/// identity's credential cannot be sent as one.
fn credentialed(
    value: SecretValue,
    binding_id: String,
    label: &str,
    named: &str,
    rejected: Unauthorized,
) -> InjectDecision {
    match InjectedHeader::new("authorization", value) {
        Ok(header) => InjectDecision::Inject(
            Injection::new(binding_id, vec![header]).on_unauthorized(rejected),
        ),
        Err(_) => refuse(
            502,
            ReasonCode::CredentialUnavailable,
            format!(
                "the credential of identity {label} for {named} is not a usable header value; replace it in Puddle"
            ),
        ),
    }
}

fn refuse(status: u16, code: ReasonCode, message: impl Into<String>) -> InjectDecision {
    InjectDecision::Refuse(InjectRefusal::new(status, code, message))
}

/// What an LFS batch request asks for: a download is a read, every other `operation` (and a body
/// that cannot be read) is a write.
///
/// The body is read as the strictest server would and the loosest: Go's `encoding/json` (Gitea
/// up to 1.21 at least) matches a key to a field ignoring case and lets a later key win, so
/// `{"operation":"download","Operation":"upload"}` is an upload there. A read is a body whose top
/// level has exactly one key that is `operation` in any case, spelled `operation`, with the value
/// `download`.
fn batch_access(body: Option<&[u8]>) -> GitAccess {
    let operation = body.and_then(|body| {
        serde::Deserializer::deserialize_map(
            &mut serde_json::Deserializer::from_slice(body),
            OnlyOperation,
        )
        .ok()
        .flatten()
    });
    if operation.as_deref() == Some("download") {
        GitAccess::Pull
    } else {
        GitAccess::Push
    }
}

/// Reads a JSON object's `operation`: `None` unless exactly one key is `operation` ignoring case
/// (Unicode lower case, so a sign that folds to an ASCII letter counts too), that key is spelled
/// `operation`, and its value is a string.
struct OnlyOperation;

impl<'de> serde::de::Visitor<'de> for OnlyOperation {
    type Value = Option<String>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an LFS batch object")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
        let (mut seen, mut operation) = (0_usize, None);
        while let Some(key) = map.next_key::<String>()? {
            if key
                .chars()
                .flat_map(char::to_lowercase)
                .eq("operation".chars())
            {
                seen += 1;
                let value = map.next_value::<serde_json::Value>()?;
                // spelled otherwise, a case-sensitive server finds no `operation` at all
                operation = (key == "operation")
                    .then(|| value.as_str().map(str::to_owned))
                    .flatten();
            } else {
                map.next_value::<serde::de::IgnoredAny>()?;
            }
        }
        Ok(if seen == 1 { operation } else { None })
    }
}

/// The credential that covers `path`: by the host the credential is for (`dev.azure.com` for every
/// Azure DevOps name), and, failing that, by the host asked for.
fn covering<'a>(git: &'a WorkspaceGit, path: &GitPath) -> CredentialChoice<'a> {
    let choice = git.credential_for(&path.credential_host, &path.owner);
    if matches!(choice, CredentialChoice::Uncovered) && path.host != path.credential_host {
        return git.credential_for(&path.host, &path.owner);
    }
    choice
}

/// `host/owner`: what a credential is chosen for. The notice that none covers it is per host: the
/// workspace picks the owners it asks for.
fn owner_key(path: &GitPath) -> String {
    format!("{}/{}", path.host, path.owner)
}

/// Which notices were raised lately.
#[derive(Debug, Default)]
struct Notices {
    last: Mutex<HashMap<String, Instant>>,
}

impl Notices {
    /// Remembers that `key` happened now; when the table is full of recent keys, the oldest
    /// makes room, so the newest is never the one forgotten.
    fn mark(&self, key: String) {
        let now = Instant::now();
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if last.len() >= MAX_NOTICES && !last.contains_key(&key) {
            last.retain(|_, at| now.duration_since(*at) < NOTICE_EVERY.max(NO_IDENTITY_GRACE));
            if last.len() >= MAX_NOTICES
                && let Some(oldest) = last
                    .iter()
                    .min_by_key(|(_, at)| **at)
                    .map(|(key, _)| key.clone())
            {
                last.remove(&oldest);
            }
        }
        last.insert(key, now);
    }

    /// Whether `key` happened after `since`.
    fn marked_since(&self, key: &str, since: Instant) -> bool {
        let last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        last.get(key).is_some_and(|at| *at >= since)
    }

    /// Whether `key` has not been raised in the last [`NOTICE_EVERY`]; counts it as raised.
    fn first(&self, key: String) -> bool {
        let now = Instant::now();
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if last.len() >= MAX_NOTICES {
            last.retain(|_, at| now.duration_since(*at) < NOTICE_EVERY);
            if last.len() >= MAX_NOTICES {
                // A flood of different notices is not a flood of events.
                return false;
            }
        }
        match last.get(&key) {
            Some(at) if now.duration_since(*at) < NOTICE_EVERY => false,
            _ => {
                last.insert(key, now);
                true
            }
        }
    }
}

#[cfg(test)]
mod tests;
