// SPDX-License-Identifier: AGPL-3.0-or-later
//! The host side of the identities screens: which accounts are already signed in on this computer,
//! whether Puddle can read a credential now, a pasted token's entry in the OS credential store, and
//! signing in on a click ([`CredentialService`]). [`HostCredentials`] runs the real sources;
//! [`FakeCredentials`] answers from what a test or the UI fixture put in it.
//!
//! No method returns a secret value. A pasted token goes in and is never read back through here;
//! the injector and the host-side API calls read it through `puddle-secrets` directly.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;
use puddle_secrets::{
    Discovery, Fetch, LastRead, ReadLog, ReadUse, Secret, SecretStore, SignInError, SignInStart,
    SignIns, SourceError, SourceSpec, Sources, StoredId, TokenScope, ToolPaths, discover,
    pasted_token,
};
use puddle_store::UserId;

/// Why a call could not be answered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CredentialsError {
    /// Not wired in this build or state (503).
    #[error("{0}")]
    Unavailable(String),
    /// A value is refused (422).
    #[error("{0}")]
    Invalid(String),
    /// Something broke; the detail is for the log (500).
    #[error("{0}")]
    Internal(String),
}

/// What reading a credential came to. Never the value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Check {
    /// The source gave a value.
    Readable,
    /// It did not, and why.
    Problem(SourceError),
}

/// What the identities screens need from the host.
pub trait CredentialService: Send + Sync {
    /// The accounts already signed in on this computer, from the fixed listing commands.
    fn found(&self) -> BoxFuture<'_, Result<Discovery, CredentialsError>>;

    /// Reads the credential once and says whether that worked. Never interactive.
    fn check(&self, source: SourceSpec) -> BoxFuture<'_, Result<Check, CredentialsError>>;

    /// Keeps a pasted token of `user` in the OS credential store, under a name that says whose it
    /// is, and returns the source that names it. The token is wiped when the call is done with it.
    fn store_token(
        &self,
        user: UserId,
        scope: TokenScope,
        token: Secret,
    ) -> BoxFuture<'_, Result<SourceSpec, CredentialsError>>;

    /// Removes a pasted token; one that is already gone counts as removed.
    fn forget_token(&self, id: StoredId) -> BoxFuture<'_, Result<(), CredentialsError>>;

    /// Starts a sign-in the user finishes outside Puddle. Only ever from a click.
    fn sign_in(&self, source: SourceSpec) -> BoxFuture<'_, Result<SignInStart, CredentialsError>>;

    /// What the last read of `source` came to, whoever asked: a workspace's request, a repository
    /// list or a check. `None` before any read since Puddle started. Reads nothing itself, and
    /// never holds a value.
    fn last_read(&self, source: &SourceSpec) -> Option<LastRead>;
}

/// The service when none is wired in: every call says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCredentials;

fn not_wired() -> CredentialsError {
    CredentialsError::Unavailable("credentials are not available in this build yet".into())
}

impl CredentialService for NoCredentials {
    fn found(&self) -> BoxFuture<'_, Result<Discovery, CredentialsError>> {
        Box::pin(async { Err(not_wired()) })
    }
    fn check(&self, _source: SourceSpec) -> BoxFuture<'_, Result<Check, CredentialsError>> {
        Box::pin(async { Err(not_wired()) })
    }
    fn store_token(
        &self,
        _user: UserId,
        _scope: TokenScope,
        _token: Secret,
    ) -> BoxFuture<'_, Result<SourceSpec, CredentialsError>> {
        Box::pin(async { Err(not_wired()) })
    }
    fn forget_token(&self, _id: StoredId) -> BoxFuture<'_, Result<(), CredentialsError>> {
        Box::pin(async { Err(not_wired()) })
    }
    fn sign_in(&self, _source: SourceSpec) -> BoxFuture<'_, Result<SignInStart, CredentialsError>> {
        Box::pin(async { Err(not_wired()) })
    }
    fn last_read(&self, _source: &SourceSpec) -> Option<LastRead> {
        None
    }
}

/// The secret store when none is wired in: every call fails, so a secret is refused (503) instead
/// of being kept somewhere it would be lost.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoSecretStore;

impl SecretStore for NoSecretStore {
    fn get(&self, _: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
        Err(puddle_secrets::StoreError)
    }

    fn set(&self, _: &StoredId, _: &Secret) -> Result<(), puddle_secrets::StoreError> {
        Err(puddle_secrets::StoreError)
    }

    fn delete(&self, _: &StoredId) -> Result<(), puddle_secrets::StoreError> {
        Err(puddle_secrets::StoreError)
    }
}

fn sign_in_error(source: &SourceSpec, err: SignInError) -> CredentialsError {
    let said = err.to_string();
    match err {
        SignInError::NothingToSignIn => CredentialsError::Invalid(match source {
            SourceSpec::AzCli { tenant, .. } => format!(
                "Puddle does not sign in to the Azure CLI; run `az login --tenant {tenant}` in a terminal"
            ),
            _ => "a pasted token has nothing to sign in to; paste a new one".into(),
        }),
        SignInError::NoPrompt(..) => {
            CredentialsError::Unavailable(format!("{said}; try again, or sign in from a terminal"))
        }
        SignInError::Ended(..) => CredentialsError::Unavailable(format!(
            "{said}; Git Credential Manager is what signs in here, so check that Git can run it"
        )),
        SignInError::Source(err) => {
            CredentialsError::Unavailable(crate::wire::source_problem(&err))
        }
        // `SignInError` grows with the Azure CLI.
        _ => CredentialsError::Unavailable(said),
    }
}

/// The real thing, over `puddle-secrets`. Cheap to share.
pub struct HostCredentials {
    tools: ToolPaths,
    sources: Sources,
    store: Arc<dyn SecretStore>,
    sign_ins: SignIns,
    reads: Arc<ReadLog>,
}

impl std::fmt::Debug for HostCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostCredentials").finish_non_exhaustive()
    }
}

impl HostCredentials {
    /// Sources that run the tools at `tools` and keep pasted tokens in `store`.
    #[must_use]
    pub fn new(tools: ToolPaths, store: Arc<dyn SecretStore>) -> Self {
        Self {
            sources: Sources::new(tools.clone(), Arc::clone(&store)),
            sign_ins: SignIns::new(tools.clone()),
            tools,
            store,
            reads: Arc::new(ReadLog::new()),
        }
    }

    /// The same, keeping the outcome of every check in `reads` and answering
    /// [`last_read`](CredentialService::last_read) from it: pass the log the secret cache of the
    /// workspaces' requests writes to, so one log holds every use.
    #[must_use]
    pub fn with_read_log(mut self, reads: Arc<ReadLog>) -> Self {
        self.reads = reads;
        self
    }

    /// The same with these sign-ins (to change how long one stays open).
    #[must_use]
    pub fn with_sign_ins(mut self, sign_ins: SignIns) -> Self {
        self.sign_ins = sign_ins;
        self
    }
}

impl CredentialService for HostCredentials {
    fn found(&self) -> BoxFuture<'_, Result<Discovery, CredentialsError>> {
        Box::pin(async move { Ok(discover(&self.tools).await) })
    }

    fn check(&self, source: SourceSpec) -> BoxFuture<'_, Result<Check, CredentialsError>> {
        Box::pin(async move {
            let check = match self.sources.fetch(&source).await {
                Ok(_) => Check::Readable,
                Err(err) => Check::Problem(err),
            };
            let problem = match &check {
                Check::Readable => None,
                Check::Problem(err) => Some(err.clone()),
            };
            self.reads.record(&source, ReadUse::Test, problem);
            Ok(check)
        })
    }

    fn store_token(
        &self,
        user: UserId,
        scope: TokenScope,
        token: Secret,
    ) -> BoxFuture<'_, Result<SourceSpec, CredentialsError>> {
        Box::pin(async move {
            let secret = pasted_token(token.expose()).map_err(|_| {
                CredentialsError::Invalid(
                    "that is not a usable token: it is empty or has spaces or control characters"
                        .into(),
                )
            })?;
            drop(token);
            // `tok-` and 128 random bits, with the user's mark.
            let id = crate::stored_ids::fresh(user, "tok").map_err(CredentialsError::Internal)?;
            let (store, key) = (Arc::clone(&self.store), id.clone());
            tokio::task::spawn_blocking(move || store.set(&key, &secret))
                .await
                .map_err(|err| CredentialsError::Internal(err.to_string()))?
                .map_err(|_| {
                    CredentialsError::Unavailable(
                        "the operating system's credential store is not available".into(),
                    )
                })?;
            Ok(SourceSpec::Stored { id, scope })
        })
    }

    fn forget_token(&self, id: StoredId) -> BoxFuture<'_, Result<(), CredentialsError>> {
        Box::pin(async move {
            let store = Arc::clone(&self.store);
            tokio::task::spawn_blocking(move || store.delete(&id))
                .await
                .map_err(|err| CredentialsError::Internal(err.to_string()))?
                .map_err(|_| {
                    CredentialsError::Unavailable(
                        "the operating system's credential store is not available".into(),
                    )
                })
        })
    }

    fn sign_in(&self, source: SourceSpec) -> BoxFuture<'_, Result<SignInStart, CredentialsError>> {
        Box::pin(async move {
            self.sign_ins
                .begin(&source)
                .await
                .map_err(|err| sign_in_error(&source, err))
        })
    }

    fn last_read(&self, source: &SourceSpec) -> Option<LastRead> {
        self.reads.last(source)
    }
}

/// Answers from what a test or the UI fixture put in it: the accounts "found", which credentials
/// are readable, the code a sign-in shows. A sign-in makes the credential readable, as a finished
/// one would. Pasted tokens are kept by id, with their value, so a test can see that one was stored
/// or removed and a fake host can answer for the token behind an id.
#[derive(Default)]
pub struct FakeCredentials {
    state: Mutex<FakeState>,
    reads: ReadLog,
}

#[derive(Default)]
struct FakeState {
    found: Option<Discovery>,
    unreadable: HashMap<SourceSpec, SourceError>,
    sign_in: SignInStart,
    tokens: HashMap<String, String>,
    next_token: u64,
    sign_ins: Vec<SourceSpec>,
}

impl std::fmt::Debug for FakeCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeCredentials").finish_non_exhaustive()
    }
}

impl FakeCredentials {
    /// Nothing found, everything readable.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What `found` answers from now on.
    pub fn set_found(&self, found: Discovery) {
        self.state().found = Some(found);
    }

    /// Makes `source` unreadable with this reason (`None` makes it readable again).
    pub fn set_unreadable(&self, source: SourceSpec, why: Option<SourceError>) {
        let mut state = self.state();
        match why {
            Some(why) => state.unreadable.insert(source, why),
            None => state.unreadable.remove(&source),
        };
    }

    /// Notes a read of `source` that a workspace's request made, as the host's secret cache does.
    pub fn note_read(&self, source: &SourceSpec, by: ReadUse, problem: Option<SourceError>) {
        self.reads.record(source, by, problem);
    }

    /// The same for a read that finished at `at`.
    pub fn note_read_at(
        &self,
        source: &SourceSpec,
        by: ReadUse,
        problem: Option<SourceError>,
        at: std::time::SystemTime,
    ) {
        self.reads.record_at(source, by, problem, at);
    }

    /// What a sign-in shows the user.
    pub fn set_sign_in(&self, start: SignInStart) {
        self.state().sign_in = start;
    }

    /// The ids of the tokens kept, in no order.
    #[must_use]
    pub fn tokens(&self) -> Vec<String> {
        let mut ids: Vec<_> = self.state().tokens.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Forgets every kept token (a UI fixture reset starts with none).
    pub fn forget_all_tokens(&self) {
        self.state().tokens.clear();
    }

    /// The value kept under `id`, for a fake host that answers by the token behind a source.
    #[must_use]
    pub fn token_value(&self, id: &str) -> Option<String> {
        self.state().tokens.get(id).cloned()
    }

    /// The sources a sign-in was started for, in order.
    #[must_use]
    pub fn sign_ins(&self) -> Vec<SourceSpec> {
        self.state().sign_ins.clone()
    }
}

impl CredentialService for FakeCredentials {
    fn found(&self) -> BoxFuture<'_, Result<Discovery, CredentialsError>> {
        Box::pin(async move { Ok(self.state().found.clone().unwrap_or_default()) })
    }

    fn check(&self, source: SourceSpec) -> BoxFuture<'_, Result<Check, CredentialsError>> {
        Box::pin(async move {
            let why = self.state().unreadable.get(&source).cloned();
            self.reads.record(&source, ReadUse::Test, why.clone());
            Ok(why.map_or(Check::Readable, Check::Problem))
        })
    }

    fn store_token(
        &self,
        user: UserId,
        scope: TokenScope,
        token: Secret,
    ) -> BoxFuture<'_, Result<SourceSpec, CredentialsError>> {
        Box::pin(async move {
            pasted_token(token.expose())
                .map_err(|_| CredentialsError::Invalid("that is not a usable token".into()))?;
            let mut state = self.state();
            state.next_token += 1;
            let id = crate::stored_ids::name(user, &format!("tok-fake-{}", state.next_token));
            state.tokens.insert(id.clone(), token.expose().to_owned());
            let id =
                StoredId::new(id).map_err(|err| CredentialsError::Internal(err.to_string()))?;
            Ok(SourceSpec::Stored { id, scope })
        })
    }

    fn forget_token(&self, id: StoredId) -> BoxFuture<'_, Result<(), CredentialsError>> {
        Box::pin(async move {
            self.state().tokens.remove(id.as_str());
            Ok(())
        })
    }

    fn sign_in(&self, source: SourceSpec) -> BoxFuture<'_, Result<SignInStart, CredentialsError>> {
        Box::pin(async move {
            if matches!(source, SourceSpec::Stored { .. } | SourceSpec::AzCli { .. }) {
                return Err(sign_in_error(&source, SignInError::NothingToSignIn));
            }
            let mut state = self.state();
            state.unreadable.remove(&source);
            state.sign_ins.push(source);
            Ok(state.sign_in.clone())
        })
    }

    fn last_read(&self, source: &SourceSpec) -> Option<LastRead> {
        self.reads.last(source)
    }
}

#[cfg(test)]
mod tests;
