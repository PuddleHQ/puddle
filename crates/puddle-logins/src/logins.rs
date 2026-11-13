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

use bytes::Bytes;
use puddle_proxy::{
    AnswerHead, AnswerRewriter, BodyField, Exchange, ExchangeRewriter, FieldKind, InjectRefusal,
    MAX_EXCHANGE_BODY, SecretValue, StandIn, StandInError, StandInOrigin, StandIns, TerminationSet,
    TokenBody,
};
use puddle_secrets::SecretStore;
use puddle_types::{Host, WorkspaceId};
use zeroize::Zeroizing;

use crate::marker::Marker;
use crate::profile::{
    Audience, Endpoint, Field, Keep, Method, Profile, Role, Scope, Slot, path_matches,
};
use crate::shape;
use crate::vault::{STORE_TIMEOUT, Vault, VaultError};

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
    /// One capture at a time, so two refreshes at once do not interleave their writes.
    capture: tokio::sync::Mutex<()>,
    /// How long a call on the credential store may take, in milliseconds.
    store_timeout_ms: AtomicU64,
    /// The stand-in of each slot that has one.
    slots: Mutex<BTreeMap<(&'static str, Slot), String>>,
    /// The earlier stand-ins of a slot whose stand-in is made again at every exchange, newest
    /// last. The tool may still hold one of them when the next exchange answers, so each stays
    /// swapped, with the real token of its own exchange, until [`RETIRED_KEPT`] newer ones exist.
    retired: Mutex<BTreeMap<(&'static str, Slot), VecDeque<String>>>,
}

/// A token found in an answer: its field, the real token, its stand-in and whether that is new.
type Planned = (&'static Field, Zeroizing<String>, String, bool);

/// How many earlier stand-ins of a slot stay swapped. A session token lasts about half an hour
/// and is exchanged again before it ends, so the tool never holds more than the last two.
pub(crate) const RETIRED_KEPT: usize = 3;

/// How many earlier stand-ins of a [`Keep::Memory`] slot stay swapped. A registry mints a token
/// per repository for each pull and the client uses it for that pull's minutes, so many pulls at
/// once (a compose file's images) each hold one; the bound keeps a guest that asks again and
/// again from growing the registry.
pub(crate) const MEMORY_KEPT: usize = 64;

/// How many earlier stand-ins of `field`'s slot stay swapped when a new one replaces them.
const fn earlier_kept(field: &Field) -> usize {
    match field.keep {
        Keep::Memory => MEMORY_KEPT,
        _ => RETIRED_KEPT,
    }
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

/// A path as a server may read it: percent-decoded, lower case, without parameters, empty and
/// `.` segments, and with `..` applied. The token endpoint is recognised by this spelling, so a
/// request that names it another way is still read.
pub(crate) fn canonical_path(path: &str) -> String {
    let path = path.split(['?', '#', ';']).next().unwrap_or_default();
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
        match segment {
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
                capture: tokio::sync::Mutex::new(()),
                store_timeout_ms: AtomicU64::new(
                    u64::try_from(STORE_TIMEOUT.as_millis()).unwrap_or(u64::MAX),
                ),
                slots: Mutex::new(BTreeMap::new()),
                retired: Mutex::new(BTreeMap::new()),
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

    /// Runs a blocking call on the credential store off the async threads, with a time limit.
    async fn store<T: Send + 'static>(
        &self,
        call: impl FnOnce(&Vault) -> Result<T, VaultError> + Send + 'static,
    ) -> Result<T, VaultError> {
        let vault = self.inner.vault.clone();
        let limit = Duration::from_millis(self.inner.store_timeout_ms.load(Ordering::Relaxed));
        match tokio::time::timeout(limit, tokio::task::spawn_blocking(move || call(&vault))).await {
            Ok(Ok(result)) => result,
            // The call was cut off or panicked: the store is not usable now.
            Ok(Err(_)) | Err(_) => Err(VaultError::Unavailable),
        }
    }

    /// Puts a pair into the registry and the slots: the stand-in the workspace holds and the real
    /// token behind it, swapped toward the hosts `scope` names. The slot's stand-in it replaces
    /// stays swapped as one of the last `keep_earlier` (none: it goes at once). The registry is the
    /// one place that checks a stand-in and a real value; what it refuses is the error.
    fn install(
        &self,
        profile: &'static Profile,
        (slot, scope): (Slot, Scope),
        stand_in: &str,
        real: &str,
        keep_earlier: usize,
    ) -> Result<(), StandInError> {
        let entry = StandIn::new(
            StandInOrigin::CapturedLogin,
            &profile.slot_name_of(slot),
            stand_in,
            SecretValue::new(real),
            profile.hosts_for(scope),
        )?;
        let old = self.slots().insert((profile.id, slot), stand_in.to_owned());
        if let Some(old) = old {
            if keep_earlier > 0 {
                self.retire(profile, slot, old, keep_earlier);
            } else {
                self.inner.registry.remove(&old);
            }
        }
        self.inner.registry.insert(entry)
    }

    /// Keeps `old` swapped as an earlier stand-in of the slot, and stops swapping the oldest ones
    /// beyond `limit`.
    fn retire(&self, profile: &Profile, slot: Slot, old: String, limit: usize) {
        let mut retired = self
            .inner
            .retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let queue = retired.entry((profile.id, slot)).or_default();
        queue.push_back(old);
        while queue.len() > limit {
            if let Some(oldest) = queue.pop_front() {
                self.inner.registry.remove(&oldest);
            }
        }
    }

    /// Reads the logins kept for the workspace and registers their stand-ins, at the workspace's
    /// start: the workspace's disk still holds the stand-ins its tool was given.
    pub async fn load(&self) {
        // With capture off nothing is registered and a login kept before is not used until
        // capture is on again. The store is only asked whether one is there, because the
        // workspace's disk still holds its stand-ins, which are now refused: that is said once.
        if !self.inner.enabled {
            self.tell_about_kept_logins().await;
            return;
        }
        let mut unreadable: Vec<&'static Profile> = Vec::new();
        let mut store_down = false;
        let mut kept: BTreeSet<String> = BTreeSet::new();
        for profile in self.inner.profiles {
            for slot in profile.slots() {
                if store_down {
                    // One call that did not answer is enough to know; the next would wait as long.
                    continue;
                }
                match self.store(move |vault| vault.read(profile, slot.0)).await {
                    Ok(Some(saved)) => {
                        kept.insert(profile.id.to_owned());
                        if self
                            .install(profile, slot, &saved.stand_in, &saved.real, 0)
                            .is_err()
                        {
                            unreadable.push(profile);
                        }
                    }
                    Ok(None) => {}
                    Err(VaultError::Damaged) => {
                        kept.insert(profile.id.to_owned());
                        unreadable.push(profile);
                    }
                    Err(VaultError::Unavailable) => store_down = true,
                }
            }
        }
        // Only an answer from every slot says what is kept.
        if !store_down {
            self.inner.marker.write(&kept);
        }
        // With the store down nothing can be kept either, so every service is told once, with
        // the words for a login that is not protected.
        let down: Vec<&'static Profile> = if store_down {
            self.inner.profiles.iter().collect()
        } else {
            Vec::new()
        };
        for (list, problem) in [
            (down, Problem::StoreUnavailable),
            (unreadable, Problem::Unreadable),
        ] {
            let mut seen: Vec<&str> = Vec::new();
            for profile in list {
                if !seen.contains(&profile.id) {
                    seen.push(profile.id);
                    self.notify(profile, problem);
                }
            }
        }
    }

    /// With capture off: tells the user, once per service, that Puddle still holds a login it
    /// kept earlier. The marker says which services those are without asking the credential
    /// store, which can wait on a locked keyring at every start. With no marker yet (a workspace
    /// from before it, or one it could not be written for) the store is asked once, the answer
    /// is written to the marker, and a store that does not answer says nothing: with capture off
    /// there is nothing to protect, and one call that did not answer is enough to know.
    async fn tell_about_kept_logins(&self) {
        if let Some(kept) = self.inner.marker.read() {
            for profile in self.inner.profiles.iter().filter(|p| kept.contains(p.id)) {
                self.notify(profile, Problem::CaptureOff);
            }
            return;
        }
        // A store that does not answer says nothing: with capture off there is nothing to protect.
        let Some(kept) = self.kept_in_store().await else {
            return;
        };
        for profile in self.inner.profiles.iter().filter(|p| kept.contains(p.id)) {
            self.notify(profile, Problem::CaptureOff);
        }
        self.inner.marker.write(&kept);
    }

    /// The services the credential store holds a login for, or `None` when it did not answer
    /// (one call that did not answer is enough to know; the next would wait as long).
    async fn kept_in_store(&self) -> Option<BTreeSet<String>> {
        let mut kept: BTreeSet<String> = BTreeSet::new();
        for profile in self.inner.profiles {
            for (slot, _) in profile.slots() {
                match self.store(move |vault| vault.holds(profile, slot)).await {
                    Ok(true) => {
                        kept.insert(profile.id.to_owned());
                        break;
                    }
                    Ok(false) => {}
                    Err(_) => return None,
                }
            }
        }
        Some(kept)
    }

    /// The services Puddle keeps a login for in this workspace, for the user to look at: names and
    /// times, never a value. The marker answers when it is known, so opening the list never waits
    /// on a locked keyring; with no marker the store is asked once and the answer is written to
    /// the marker. A store that does not answer gives an empty list that says so
    /// ([`KeptList::store_answered`]).
    pub async fn kept_list(&self) -> KeptList {
        let since = match self.inner.marker.read_since() {
            Some(since) => since,
            None => match self.kept_in_store().await {
                Some(kept) => {
                    self.inner.marker.write(&kept);
                    // What the marker now says; where it cannot be written, no time is known.
                    self.inner
                        .marker
                        .read_since()
                        .unwrap_or_else(|| kept.into_iter().map(|id| (id, None)).collect())
                }
                None => {
                    return KeptList {
                        logins: Vec::new(),
                        store_answered: false,
                    };
                }
            },
        };
        let logins = self
            .inner
            .profiles
            .iter()
            .filter_map(|profile| {
                since.get(profile.id).map(|since_ms| KeptLogin {
                    profile: profile.id,
                    name: profile.name,
                    since_ms: *since_ms,
                })
            })
            .collect();
        KeptList {
            logins,
            store_answered: true,
        }
    }

    /// What the marker says is kept, as a sorted list (tests).
    #[cfg(test)]
    pub(crate) fn inner_marker_kept(&self) -> Option<Vec<String>> {
        self.inner
            .marker
            .read()
            .map(|ids| ids.into_iter().collect())
    }

    /// Adds `profile` to the marker when it is known. A marker that is not known stays so: the
    /// next start with capture on asks the store and writes all of it.
    fn mark(&self, profile: &Profile, kept: bool) {
        if let Some(mut ids) = self.inner.marker.read() {
            let changed = if kept {
                ids.insert(profile.id.to_owned())
            } else {
                ids.remove(profile.id)
            };
            if changed {
                self.inner.marker.write(&ids);
            }
        }
    }

    /// The logins Puddle holds for this workspace (the ones registered since its start), without
    /// any value.
    #[must_use]
    pub fn kept(&self) -> Vec<Kept> {
        let slots = self.slots();
        self.inner
            .profiles
            .iter()
            .filter_map(|profile| {
                let mut roles: Vec<Role> = profile
                    .slots()
                    .into_iter()
                    .filter(|(slot, _)| slots.contains_key(&(profile.id, *slot)))
                    .map(|(slot, _)| slot.role)
                    .collect();
                roles.dedup();
                (!roles.is_empty()).then_some(Kept {
                    profile: profile.id,
                    name: profile.name,
                    roles,
                })
            })
            .collect()
    }

    /// Forgets `profile`'s login for this workspace: its stand-ins stop being swapped (the tool
    /// will ask for a new sign-in) and its real tokens are deleted from the credential store.
    /// `false` when the profile is not one of this workspace's.
    ///
    /// # Errors
    /// [`ForgetError::Store`] when the credential store did not delete a token; the stand-ins are
    /// already gone, and trying again deletes what is left.
    pub async fn forget(&self, profile_id: &str) -> Result<bool, ForgetError> {
        let Some(profile) = self
            .inner
            .profiles
            .iter()
            .find(|p| p.id == profile_id || p.name == profile_id)
        else {
            return Ok(false);
        };
        let mut failed = false;
        for (slot, _) in profile.memory_slots() {
            self.unregister(profile, slot);
        }
        for (slot, _) in profile.slots() {
            self.unregister(profile, slot);
            failed |= self
                .store(move |vault| vault.delete(profile, slot))
                .await
                .is_err();
        }
        if failed {
            Err(ForgetError::Store)
        } else {
            self.mark(profile, false);
            Ok(true)
        }
    }

    /// Stops swapping the stand-ins of `slot`, its current one and the earlier ones.
    fn unregister(&self, profile: &Profile, slot: Slot) {
        if let Some(stand_in) = self.slots().remove(&(profile.id, slot)) {
            self.inner.registry.remove(&stand_in);
        }
        let earlier = self
            .inner
            .retired
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(profile.id, slot));
        for stand_in in earlier.into_iter().flatten() {
            self.inner.registry.remove(&stand_in);
        }
    }

    /// Deletes every token kept for `workspace`, when the workspace is deleted. Blocking. Returns
    /// how many deletions the store refused (the rest are gone).
    #[must_use]
    pub fn delete_workspace(
        store: &Arc<dyn SecretStore>,
        workspace: &WorkspaceId,
        profiles: &[Profile],
    ) -> usize {
        let vault = Vault::new(Arc::clone(store), workspace);
        profiles
            .iter()
            .flat_map(|profile| {
                profile
                    .slots()
                    .into_iter()
                    .map(move |(slot, _)| (profile, slot))
            })
            .filter(|(profile, slot)| vault.delete(profile, *slot).is_err())
            .count()
    }

    /// Captures one token answer: keeps the real tokens, and gives back the body with stand-ins
    /// in their place. `Ok(None)`: nothing in the answer to capture.
    async fn capture(
        &self,
        route: (&'static Profile, &'static Endpoint),
        body: &[u8],
        content_type: Option<&str>,
    ) -> Result<Option<Bytes>, Problem> {
        let (profile, endpoint) = route;
        let mut parsed =
            TokenBody::parse(content_type, body).map_err(|_| Problem::UnexpectedAnswer)?;
        if holds_unnamed_token(endpoint, &parsed.fields()) {
            return Err(Problem::UnnamedToken);
        }
        let mut found: Vec<(&'static Field, Zeroizing<String>)> = Vec::new();
        for field in endpoint.fields {
            // The first field that takes the token is the one used for its answer field. Fields
            // with an audience share a path on purpose (one per resource): `by_audience` picks.
            if field.audience == Audience::Any
                && found
                    .iter()
                    .any(|(taken, _)| taken.path == field.path && taken.audience == Audience::Any)
            {
                continue;
            }
            let Some(text) = parsed.text_at(field.path).filter(|text| !text.is_empty()) else {
                continue;
            };
            if field.only_prefixed && !field.prefixes.iter().any(|p| text.starts_with(p)) {
                continue;
            }
            found.push((field, Zeroizing::new(text.into_owned())));
        }
        if found.is_empty() {
            // A pending device code, an error, a refusal: nothing to keep.
            return Ok(None);
        }
        let found = by_audience(found, &parsed);
        if found.is_empty() {
            // The answer has a token that no field of the profile takes: it must not pass as if
            // it were kept.
            return Err(Problem::UnusableToken);
        }
        if parsed
            .text("token_type")
            .is_some_and(|kind| kind.eq_ignore_ascii_case("dpop"))
        {
            return Err(Problem::BoundToken);
        }
        if found
            .iter()
            .any(|(field, real)| !shape::usable_for(field, real))
        {
            return Err(Problem::UnusableToken);
        }
        let _one_at_a_time = self.inner.capture.lock().await;
        let planned = self.plan(profile, found)?;
        // The credential store first: a token that was not kept is not replaced in the answer.
        let mut stored = false;
        for (field, real, stand_in, _) in &planned {
            if field.keep != Keep::Stored {
                continue;
            }
            let slot = field.slot();
            let (stand_in, real) = (stand_in.clone(), real.clone());
            self.store(move |vault| vault.write(profile, slot, &stand_in, &real))
                .await
                .map_err(|_| Problem::StoreUnavailable)?;
            stored = true;
        }
        if stored {
            self.mark(profile, true);
        }
        for (field, real, stand_in, is_new) in &planned {
            let slot = (field.slot(), field.scope);
            // A refresh gives the stand-in a new token without a moment in which it has none; a
            // slot the registry has lost (a forgotten login) is made again.
            let refreshed = !is_new
                && self
                    .inner
                    .registry
                    .set_real(stand_in, SecretValue::new(real.as_str()))
                    == Ok(true);
            if !refreshed {
                let keep_earlier = if shape::renews(real, field) {
                    earlier_kept(field)
                } else {
                    0
                };
                self.install(profile, slot, stand_in, real, keep_earlier)
                    .map_err(|_| Problem::UnusableToken)?;
            }
            parsed.set_text_at(field.path, stand_in);
            tracing::info!(
                workspace = %self.inner.workspace,
                profile = profile.id,
                role = field.role.key(),
                new = *is_new,
                "kept a login's token and gave the workspace a stand-in"
            );
        }
        Ok(Some(Bytes::copy_from_slice(&parsed.render())))
    }

    /// The stand-in each found token gets, and whether it is new: the slot's own for a token that
    /// keeps it, the one an earlier field of the same answer gave the same token, or a new one.
    fn plan(
        &self,
        profile: &'static Profile,
        found: Vec<(&'static Field, Zeroizing<String>)>,
    ) -> Result<Vec<Planned>, Problem> {
        let mut planned: Vec<Planned> = Vec::new();
        for (field, real) in found {
            // Two fields of one answer with the same token in one slot (a registry answers with
            // `token` and `access_token`) give it one stand-in.
            let in_answer = planned
                .iter()
                .find(|(taken, kept, _, _)| {
                    taken.slot() == field.slot() && kept.as_str() == real.as_str()
                })
                .map(|(_, _, stand_in, _)| stand_in.clone());
            // A session token's fields and a JWT's claims hold its expiry, so a new token gets a
            // new stand-in; any other token keeps the slot's.
            let existing = in_answer.or_else(|| {
                self.slots()
                    .get(&(profile.id, field.slot()))
                    .cloned()
                    .filter(|_| !shape::renews(&real, field))
            });
            let planned_stand_in = match existing {
                Some(stand_in) => (stand_in, false),
                // The system had no random numbers, or the token cannot be copied: either way
                // there is no stand-in to give.
                None => (
                    shape::stand_in_for(&real, field).map_err(|_| Problem::UnusableToken)?,
                    true,
                ),
            };
            planned.push((field, real, planned_stand_in.0, planned_stand_in.1));
        }
        Ok(planned)
    }
}

/// The fields among `found` that this answer is for: the ones with no condition, the first
/// [`Audience::Named`] field of each path whose resource the answer names, and the
/// [`Audience::Unnamed`] ones of a path no named field took.
fn by_audience<'f>(
    found: Vec<(&'f Field, Zeroizing<String>)>,
    parsed: &TokenBody,
) -> Vec<(&'f Field, Zeroizing<String>)> {
    let answer_hints = {
        let mut text = String::new();
        for name in ["scope", "resource"] {
            if let Some(value) = parsed.text(name) {
                text.push_str(&value);
                text.push(' ');
            }
        }
        text
    };
    let mut named: BTreeSet<&str> = BTreeSet::new();
    let mut taken = Vec::new();
    let mut unnamed = Vec::new();
    for (field, real) in found {
        match field.audience {
            Audience::Any => taken.push((field, real)),
            Audience::Named(needles) => {
                if named.contains(field.path) {
                    continue;
                }
                let mut hints = answer_hints.clone();
                hints.push_str(&shape::audience_of(&real).unwrap_or_default());
                let hints = hints.to_ascii_lowercase();
                if needles.iter().any(|needle| hints.contains(needle)) {
                    named.insert(field.path);
                    taken.push((field, real));
                }
            }
            Audience::Unnamed => unnamed.push((field, real)),
        }
    }
    taken.extend(unnamed.into_iter().filter(|(f, _)| !named.contains(f.path)));
    taken
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

/// The shortest text in a token-like field that counts as a token: shorter ones are lifetimes,
/// counts and flags that happen to carry the word in their name.
const MIN_TOKEN_LEN: usize = 16;

/// Name endings of fields that carry a count, a kind or an address, not a credential.
const NOT_TOKENS: [&str; 8] = [
    "_type",
    "_in",
    "_at",
    "_url",
    "_uri",
    "_endpoint",
    "_scope",
    "_label",
];

/// Whether a field's name says it holds a credential.
fn token_like(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if NOT_TOKENS.iter().any(|suffix| name.ends_with(suffix)) {
        return false;
    }
    name.contains("token")
        || name.contains("secret")
        || name.contains("password")
        || name.ends_with("key")
}

/// Whether the answer holds a credential Puddle would not replace: a token-like text in a field
/// the endpoint does not name (at any depth), or a named field that is repeated or structured
/// (the tool may read the one Puddle did not).
fn holds_unnamed_token(endpoint: &Endpoint, fields: &[BodyField]) -> bool {
    let named = |field: &BodyField| endpoint.fields.iter().any(|f| f.path == field.path);
    fields.iter().any(|field| {
        if named(field) {
            let repeated = fields.iter().filter(|f| f.path == field.path).count() > 1;
            return repeated || field.kind == FieldKind::Structured;
        }
        matches!(field.kind, FieldKind::Text(n) if n >= MIN_TOKEN_LEN) && token_like(&field.name)
    })
}

/// What the tool is told when its login is refused: the reason, and what to do.
fn refusal_for(profile: &Profile, problem: Problem) -> InjectRefusal {
    let why = match problem {
        Problem::StoreUnavailable => {
            "the credential store is not available (locked or not running); unlock it or start it and sign in again"
        }
        Problem::UnexpectedAnswer => {
            "the sign-in answer is in a form Puddle does not read (compressed, too large or not JSON or a form)"
        }
        Problem::UnusableToken => "the token has a form Puddle cannot give a stand-in for",
        Problem::BoundToken => "the service ties its token to a key of the tool's own",
        Problem::UnnamedToken => "the answer carries a token in a field Puddle does not know",
        _ => "Puddle could not keep it",
    };
    InjectRefusal::new(
        502,
        "login_not_kept",
        format!(
            "Puddle did not let the {} sign-in through: {why}. Nothing reached this workspace; Puddle's notice says what to do.",
            profile.name
        ),
    )
}

/// The answer of one token endpoint, being captured.
struct Capture {
    logins: Logins,
    profile: &'static Profile,
    endpoint: &'static Endpoint,
    /// Set when the answer could not be kept: the tool then gets this and not the answer.
    refusal: Option<InjectRefusal>,
}

impl Capture {
    fn refuse(&mut self, problem: Problem) {
        self.logins.notify(self.profile, problem);
        self.refusal = Some(refusal_for(self.profile, problem));
    }
}

impl AnswerRewriter for Capture {
    fn wants(&mut self, head: &AnswerHead<'_>) -> bool {
        // Only a success answer has tokens; an error answer is the tool's to read.
        if !(200..300).contains(&head.status) || head.status == 204 {
            return false;
        }
        let identity = head
            .content_encoding
            .is_none_or(|e| e.trim().is_empty() || e.trim().eq_ignore_ascii_case("identity"));
        let small = head
            .content_length
            .is_none_or(|n| n <= MAX_EXCHANGE_BODY as u64);
        if TokenBody::reads(head.content_type) && identity && small {
            return true;
        }
        self.refuse(Problem::UnexpectedAnswer);
        false
    }

    fn too_large(&mut self) {
        self.refuse(Problem::UnexpectedAnswer);
    }

    fn refusal(&mut self) -> Option<InjectRefusal> {
        self.refusal.take()
    }

    fn rewrite<'a>(
        &'a mut self,
        head: &'a AnswerHead<'a>,
        body: &'a [u8],
    ) -> puddle_proxy::BoxFuture<'a, Option<Bytes>> {
        Box::pin(async move {
            match self
                .logins
                .capture((self.profile, self.endpoint), body, head.content_type)
                .await
            {
                Ok(replaced) => replaced,
                Err(problem) => {
                    self.refuse(problem);
                    None
                }
            }
        })
    }
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
            refusal: None,
        })))
    }
}

#[cfg(test)]
mod refusal_tests;
