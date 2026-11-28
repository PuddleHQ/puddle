// SPDX-License-Identifier: AGPL-3.0-or-later
//! One workspace's captured logins: what the token endpoints answer, what is kept, what the
//! workspace holds instead.
//!
//! The workspace's tool logs in as it always does. When the token endpoint of a profile answers
//! with tokens, the real ones go to the operating system's credential store and the answer the
//! tool receives carries stand-ins with the tokens' shape. When the tool refreshes, it sends the
//! stand-in of its refresh token; the proxy swaps the real one into that one field of the request
//! ([`puddle_proxy::Exchange::swapping`]) and the new answer is captured the same way, behind the
//! same stand-in: a stand-in names a slot (a profile's role in this workspace), so a refresh
//! changes the real token behind it and nothing the tool has stored.
//!
//! Where the stand-ins are swapped back is the proxy's ([`puddle_proxy::StandIns`]): in the headers
//! of requests to the profile's hosts, and nowhere else.
//!
//! If a login cannot be captured (the answer is not in a form Puddle reads, the token is bound to
//! a key, the credential store is not there, the answer carries a token the profile does not
//! name) the login fails: the tool gets a `502` that names the reason, never the real answer, and
//! the user is told. A real token never reaches the workspace by this path.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_proxy::{Exchange, ExchangeRewriter, StandIns, TerminationSet};
use puddle_secrets::SecretStore;
use puddle_types::{Host, WorkspaceId};

use crate::marker::Marker;
use crate::profile::{Endpoint, Method, Profile, Role, Slot, path_matches};
use crate::vault::{STORE_TIMEOUT, Vault};

/// What went wrong with a login, for the notice the user sees. Names only: no value is in any of
/// these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Problem {
    /// The operating system's credential store did not take the token, so the login failed.
    StoreUnavailable,
    /// The service's answer was not in a form Puddle reads (not JSON or a form, compressed, or
    /// larger than a token answer is), so the login failed.
    UnexpectedAnswer,
    /// A token cannot be copied: a length or a character a stand-in cannot have. The login failed.
    UnusableToken,
    /// The service ties its tokens to a key of the tool's own (`DPoP`), which a stand-in cannot
    /// stand in for. The login failed.
    BoundToken,
    /// The answer carries a token in a field the profile does not name (a new `id_token`, a
    /// repeated field, a nested one), so Puddle could not keep it and the login failed.
    UnnamedToken,
    /// A login Puddle kept earlier could not be read back at the workspace's start; the tool will
    /// ask for a new sign-in.
    Unreadable,
    /// Capture is off for the workspace, but Puddle still holds a login it kept while capture
    /// was on: the stand-ins in the workspace are no longer swapped, so the services refuse them.
    CaptureOff,
}

/// Where a [`Problem`] goes. Implementations must not block.
pub trait LoginNotices: Send + Sync {
    /// Tells the user that `profile`'s login in `workspace` has `problem`.
    fn problem(&self, workspace: &WorkspaceId, profile: &Profile, problem: Problem);
}

/// A login Puddle holds for a workspace, as the user is shown it: no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kept {
    /// The profile's short name (`claude`).
    pub profile: &'static str,
    /// What the user calls the service (`Claude Code`).
    pub name: &'static str,
    /// The tokens held.
    pub roles: Vec<Role>,
}

/// A service Puddle keeps a login for in a workspace, as the user is shown it: no value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptLogin {
    /// The profile's short name (`claude`).
    pub profile: &'static str,
    /// What the user calls the service (`Claude Code`).
    pub name: &'static str,
    /// Epoch ms Puddle first noted the login as kept, when it knows.
    pub since_ms: Option<u64>,
}

/// The logins kept for a workspace, as [`Logins::kept_list`] answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptList {
    /// The services with a login kept, in the order of the profiles.
    pub logins: Vec<KeptLogin>,
    /// `false` when the credential store did not answer and no marker could say instead: the
    /// list is then empty because it is not known, not because nothing is kept.
    pub store_answered: bool,
}

/// A token endpoint of a profile and its host, ready to match requests.
struct Route {
    host: Host,
    /// The endpoint's path as [`canonical_path`] spells it.
    path: String,
    method: Method,
    profile: &'static Profile,
    endpoint: &'static Endpoint,
}

struct Inner {
    workspace: WorkspaceId,
    enabled: bool,
    profiles: &'static [Profile],
    routes: Vec<Route>,
    registry: Arc<StandIns>,
    vault: Vault,
    notices: Arc<dyn LoginNotices>,
    marker: Marker,
    /// One change of the kept logins at a time (a capture, a forget, the start's load), so two
    /// refreshes at once do not interleave their writes and a login forgotten during a capture
    /// does not come back when the capture ends.
    capture: Arc<tokio::sync::Mutex<()>>,
    /// How long a call on the credential store may take, in milliseconds.
    store_timeout_ms: AtomicU64,
    /// Held by the blocking thread of each store call until the call returns, so a call that was
    /// given up on keeps the next one waiting: its write cannot land after a later one.
    store_gate: Arc<tokio::sync::Mutex<()>>,
    /// How many store calls were given up on.
    store_timeouts: AtomicU64,
    /// The stand-in of each slot that has one.
    slots: Mutex<BTreeMap<(&'static str, Slot), String>>,
    /// The earlier stand-ins of a slot whose stand-in is made again at every exchange, newest
    /// last. The tool may still hold one of them when the next exchange answers, so each stays
    /// swapped, with the real token of its own exchange, until [`RETIRED_KEPT`] newer ones exist.
    retired: Mutex<BTreeMap<(&'static str, Slot), VecDeque<String>>>,
    /// The profiles whose slots the start's load could not read because the store did not answer
    /// and that have not been asked again: the first exchange of each one does.
    unread: Mutex<BTreeSet<&'static str>>,
}

/// One workspace's captured logins. Cheap to clone.
#[derive(Clone)]
pub struct Logins {
    inner: Arc<Inner>,
}

impl fmt::Debug for Logins {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Neither stand-ins nor tokens: only what is configured.
        f.debug_struct("Logins")
            .field("workspace", &self.inner.workspace)
            .field("enabled", &self.inner.enabled)
            .field("profiles", &self.inner.profiles.len())
            .finish_non_exhaustive()
    }
}

/// A path as a server may read it: percent-decoded, lower case, without the query, without the
/// `;parameters` of each segment, without empty and `.` segments, and with `..` applied. The
/// token endpoint is recognised by this spelling, so a request that names it another way is
/// still read.
pub(crate) fn canonical_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        let hex = |i: usize| bytes.get(i).and_then(|b| char::from(*b).to_digit(16));
        if let (b'%', Some(hi), Some(lo)) = (byte, hex(at + 1), hex(at + 2)) {
            decoded.push(u8::try_from(hi * 16 + lo).unwrap_or(b'/'));
            at += 3;
        } else {
            decoded.push(byte);
            at += 1;
        }
    }
    let decoded = String::from_utf8_lossy(&decoded).to_ascii_lowercase();
    let mut segments: Vec<&str> = Vec::new();
    for segment in decoded.split(['/', '\\']) {
        match segment.split(';').next().unwrap_or_default() {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    format!("/{}", segments.join("/"))
}

/// The built-in token endpoint a request to `host` with `method` and `path` is for, as the
/// proxy reads it (the path in any spelling `canonical_path` collapses). A test that stands in
/// for a service uses it to know which of its answers capture will read.
#[must_use]
pub fn token_endpoint_of(
    host: &str,
    method: &str,
    path: &str,
) -> Option<(&'static Profile, &'static Endpoint)> {
    let path = canonical_path(path);
    crate::profile::builtin().iter().find_map(|profile| {
        profile
            .endpoints
            .iter()
            .find(|e| {
                e.host.eq_ignore_ascii_case(host)
                    && e.method.accepts(method)
                    && path_matches(&canonical_path(e.path), &path)
            })
            .map(|endpoint| (profile, endpoint))
    })
}

impl Logins {
    /// The captured logins of `workspace`, for the profiles `profiles`.
    ///
    /// `registry` is the workspace's stand-in registry (the one its termination swaps from) and
    /// `store` the operating system's credential store. With `enabled` false nothing is captured
    /// and nothing is swapped: the workspace's logins stay in the workspace. `marker` is the file
    /// that says which services have a login kept, so that a start with capture off does not ask
    /// the store.
    #[must_use]
    pub fn new(
        workspace: WorkspaceId,
        enabled: bool,
        profiles: &'static [Profile],
        registry: Arc<StandIns>,
        store: Arc<dyn SecretStore>,
        notices: Arc<dyn LoginNotices>,
        marker: Marker,
    ) -> Self {
        let routes = profiles
            .iter()
            .flat_map(|profile| {
                profile.endpoints.iter().filter_map(move |endpoint| {
                    // The literals of the profiles; a test checks each parses.
                    let host = Host::parse_normalised(endpoint.host).ok()?;
                    Some(Route {
                        host,
                        path: canonical_path(endpoint.path),
                        method: endpoint.method,
                        profile,
                        endpoint,
                    })
                })
            })
            .collect();
        Self {
            inner: Arc::new(Inner {
                vault: Vault::new(store, &workspace),
                workspace,
                enabled,
                profiles,
                routes,
                registry,
                notices,
                marker,
                capture: Arc::new(tokio::sync::Mutex::new(())),
                store_timeout_ms: AtomicU64::new(
                    u64::try_from(STORE_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
                ),
                store_gate: Arc::default(),
                store_timeouts: AtomicU64::new(0),
                slots: Mutex::new(BTreeMap::new()),
                retired: Mutex::new(BTreeMap::new()),
                unread: Mutex::new(BTreeSet::new()),
            }),
        }
    }

    /// Gives up on a call on the credential store after `limit` instead of the default ten seconds
    /// (a locked store can wait for the user to unlock it). A call that is given up on counts as
    /// the store being unavailable.
    pub fn set_store_timeout(&self, limit: Duration) {
        self.inner.store_timeout_ms.store(
            u64::try_from(limit.as_millis()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
    }

    /// The hosts the workspace decrypts for its logins: every profile's token endpoints and API
    /// hosts while capture is on, none while it is off.
    #[must_use]
    pub fn hosts(&self) -> TerminationSet {
        let mut set = TerminationSet::new();
        if self.inner.enabled {
            for profile in self.inner.profiles {
                set.extend(&profile.hosts());
            }
        }
        set
    }

    fn slots(&self) -> std::sync::MutexGuard<'_, BTreeMap<(&'static str, Slot), String>> {
        self.inner
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn notify(&self, profile: &Profile, problem: Problem) {
        tracing::warn!(
            workspace = %self.inner.workspace,
            profile = profile.id,
            ?problem,
            "a login was refused"
        );
        self.inner
            .notices
            .problem(&self.inner.workspace, profile, problem);
    }
}

/// Why a login could not be forgotten completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ForgetError {
    /// The credential store did not delete a token.
    #[error(
        "the operating system's credential store did not delete a saved login; unlock it or start it and ask again (what was already deleted stays deleted)"
    )]
    Store,
}

impl ExchangeRewriter for Logins {
    fn is_token_endpoint(&self, host: &Host) -> bool {
        self.inner.enabled && self.inner.routes.iter().any(|route| route.host == *host)
    }

    fn begin(&self, host: &Host, method: &str, path: &str) -> Option<Exchange> {
        if !self.inner.enabled {
            return None;
        }
        // The host first: most requests on a decrypted host are not to a token endpoint, and
        // only these pay for the path's canonical spelling.
        let mut on_host = self
            .inner
            .routes
            .iter()
            .filter(|route| route.host == *host)
            .peekable();
        on_host.peek()?;
        let path = canonical_path(path);
        let route = on_host
            .find(|route| route.method.accepts(method) && path_matches(&route.path, &path))?;
        let mut exchange = Exchange::new();
        if self.unread().remove(route.profile.id) {
            let logins = self.clone();
            let profile = route.profile;
            exchange = exchange.after(Box::pin(async move { logins.read_again(profile).await }));
        }
        for field in route.endpoint.request_fields {
            exchange = exchange.swapping(*field);
        }
        // An endpoint with no token fields (a revoke) is only a request to swap in: its answer
        // is not read, so an empty `200` is not an unexpected answer.
        if route.endpoint.fields.is_empty() {
            return Some(exchange);
        }
        Some(exchange.answered_by(Box::new(Capture {
            logins: self.clone(),
            profile: route.profile,
            endpoint: route.endpoint,
        })))
    }
}

mod capture;
mod refusal;
mod store;

use capture::Capture;
#[cfg(test)]
pub(crate) use store::{MEMORY_KEPT, RETIRED_KEPT};
