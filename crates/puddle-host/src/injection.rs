// SPDX-License-Identifier: GPL-3.0-or-later
//! The host's side of credential injection: one CA for each running sandbox, the registry that
//! tells the proxy which hosts each workspace decrypts, the injector of each start, the cache of
//! secrets behind it, and the stand-ins of the workspace's environment secrets.
//!
//! A sandbox's CA is made when the sandbox starts and dropped when it stops: it lives in this
//! process's memory only, and its certificate goes into the guest's trust at boot. The CA has no
//! name constraint, so the hosts a workspace decrypts are only what its [`Termination`] says, and
//! that is recomputed whenever the workspace's identities or secrets change: a credential or a
//! secret for a host the workspace has not used yet applies to the next connection, with no
//! restart.
//!
//! The registry of a workspace's stand-ins is built from the store when it starts and kept for as
//! long as it runs; a change to its environment brings the registry and the decrypted hosts up to
//! date, while the guest's own environment is read at its next start.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use puddle_ca::{CaBuilder, CaCertificate, WorkspaceCa};
use puddle_inject::{CredentialSource, GitInjector, StoreSettings};
use puddle_proxy::{
    GitSshHostKeys, Injector, Interception, PassThrough, RequestRule, RequestRules, StandInOrigin,
    StandIns, Termination, TerminationSet, TerminationSource, Terminations,
};
use puddle_secrets::{Fetch, ReadLog, SecretCache, SecretStore, SourceSpec, Sources};
use puddle_store::{Store, WorkspaceGit};
use puddle_types::{
    CURATED_PASS_THROUGH, Event, EventSink, GuestEnv, InterceptionMode, RuleId, WorkspaceId,
};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use puddle_logins::Logins;
use puddle_proxy::ForeignCredentialWarning;

use crate::environment::{Resolved, resolve};
use crate::git_hosts::{SshListing, decrypt_set, ssh_credential_hosts};
use crate::logins::LoginKeeper;

/// What an injector is built from.
#[derive(Clone)]
#[non_exhaustive]
pub struct InjectorInputs {
    /// The database: identities, and each workspace's Git settings.
    pub store: Arc<Store>,
    /// The host's one cache of secrets read from the user's own sign-ins (`gh`, Git, the operating
    /// system's store). Every workspace shares it, so one source is read once at a time.
    pub secrets: Arc<SecretCache<Sources>>,
    /// Where a notice for the user goes (a sign-in that is needed, a push that was refused).
    pub events: Arc<dyn EventSink>,
}

impl std::fmt::Debug for InjectorInputs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InjectorInputs").finish_non_exhaustive()
    }
}

/// Builds the injector that decides, for each request on a host a workspace decrypts, which
/// credential is added. Called once for each start of a workspace's sandbox, with that workspace.
pub type InjectorFactory =
    Arc<dyn Fn(&InjectorInputs, &WorkspaceId) -> Arc<dyn Injector> + Send + Sync>;

/// The injector a workspace gets when the host was given no factory: Puddle's own, for Git hosts.
/// It reads the workspace's identities and lists from the store for each request, and the secret
/// of an identity from the cache at the moment it is needed.
fn git_injector(inputs: &InjectorInputs, workspace: &WorkspaceId) -> Arc<dyn Injector> {
    let settings = StoreSettings::new(Arc::clone(&inputs.store), workspace.clone());
    let credentials: Arc<dyn CredentialSource> = inputs.secrets.clone();
    Arc::new(GitInjector::new(
        workspace.clone(),
        Arc::new(settings),
        credentials,
        Arc::clone(&inputs.events),
    ))
}

/// What a sandbox needs from [`Injection::begin`].
pub(crate) struct Began {
    /// The CA's public certificate, for the guest's trust.
    pub(crate) certificate: CaCertificate,
    /// The workspace's Git settings as they were when its CA was made.
    pub(crate) git: WorkspaceGit,
    /// This start's SSH host keys and the names they are listed under, when Git over SSH is on.
    pub(crate) ssh: Option<SshListing>,
    /// The variables the guest starts with: the workspace's own and the global ones, each secret
    /// as its stand-in.
    pub(crate) env: GuestEnv,
}

/// What a running sandbox holds in memory: its CA, the injector made for this start, the
/// registry of its stand-ins and its captured logins.
#[derive(Clone)]
struct Running {
    ca: Arc<WorkspaceCa>,
    /// This start's SSH host keys; only when Git over SSH is on.
    ssh_keys: Option<Arc<GitSshHostKeys>>,
    injector: Arc<dyn Injector>,
    stand_ins: Arc<StandIns>,
    logins: Logins,
    /// The hosts this start's tools refused the certificate for; kept across changes to the
    /// workspace's settings.
    learned: Arc<PassThrough>,
    /// Whether this workspace wants the warning about a credential Puddle did not add, and what
    /// it was told already; kept across changes to its settings.
    foreign_warning: Arc<ForeignCredentialWarning>,
}

impl Running {
    /// What the workspace's termination is made of, around `set`: the registry of its stand-ins
    /// (secrets and captured logins share it) and the reading of its login exchanges.
    fn termination(
        &self,
        git: &WorkspaceGit,
        set: TerminationSet,
        interception: Interception,
        request_rules: RequestRules,
    ) -> Termination {
        let termination = Termination::new(set, Arc::clone(&self.ca), Arc::clone(&self.injector))
            .with_stand_ins(Arc::clone(&self.stand_ins))
            .with_exchanges(Arc::new(self.logins.clone()))
            .with_interception(interception)
            .with_request_rules(request_rules)
            .with_foreign_credential_warning(Arc::clone(&self.foreign_warning));
        // The names register whether or not Puddle serves SSH: with it off, SSH to a credential's
        // host is refused as the spec says, not decided by the rules and spliced raw. Only the
        // keys depend on the option.
        let termination = termination.with_git_ssh_hosts(ssh_credential_hosts(git));
        match &self.ssh_keys {
            Some(keys) => termination.with_git_ssh_keys(Arc::clone(keys)),
            None => termination,
        }
    }
}

/// What the workspace decrypts: its identities' credential hosts, the hosts of its secrets and
/// the hosts of the logins it captures.
fn decrypted(git: &WorkspaceGit, stand_ins: &StandIns, logins: &Logins) -> TerminationSet {
    let mut set = decrypt_set(git);
    set.extend(&stand_ins.hosts());
    set.extend(&logins.hosts());
    set
}

/// How long a workspace's secrets may take to read from the credential store. A locked keyring
/// waits for the user to unlock it; past this the start fails and says so, instead of hanging.
const VAULT_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct Injection {
    terminations: Arc<Terminations>,
    inputs: InjectorInputs,
    /// Without one, each workspace gets [`git_injector`].
    factory: Option<InjectorFactory>,
    /// Where the real values of environment secrets are kept.
    vault: Arc<dyn SecretStore>,
    /// Makes each start's captured logins over the same credential store.
    login_keeper: LoginKeeper,
    running: Mutex<BTreeMap<WorkspaceId, Running>>,
    /// Held while a workspace's settings are read and what it decrypts is changed to match, so
    /// the last change to run is the last one to read: a slow reader never puts back an old set.
    syncing: Mutex<()>,
    /// One lock for each workspace, held while its environment is read (the credential store can
    /// take a while) and applied or registered, so a start and a change to the environment never
    /// cross: whichever comes second reads what the first left. Per workspace, so a credential
    /// store that is slow for one workspace's secrets holds up no other workspace.
    environment: Mutex<BTreeMap<WorkspaceId, Arc<tokio::sync::Mutex<()>>>>,
    vault_timeout: Duration,
    /// Whether each start makes SSH host keys for Git over SSH.
    git_ssh: bool,
    /// Where the warning setting is read; without it the warning is on.
    settings: Option<Arc<dyn puddle_api::SettingsRepo>>,
}

impl Injection {
    pub(crate) fn new(
        terminations: Arc<Terminations>,
        inputs: InjectorInputs,
        factory: Option<InjectorFactory>,
        vault: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            terminations,
            login_keeper: LoginKeeper::new(Arc::clone(&vault), Arc::clone(&inputs.events)),
            inputs,
            factory,
            vault,
            running: Mutex::new(BTreeMap::new()),
            syncing: Mutex::new(()),
            environment: Mutex::new(BTreeMap::new()),
            vault_timeout: VAULT_TIMEOUT,
            git_ssh: false,
            settings: None,
        }
    }

    /// Reads the warning about a credential Puddle did not add from `settings`, global and the
    /// workspace's own, each time one is found.
    pub(crate) fn with_settings(mut self, settings: Arc<dyn puddle_api::SettingsRepo>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// `workspace`'s warning: on by its setting, else the global one, else on.
    fn foreign_warning(&self, workspace: &WorkspaceId) -> ForeignCredentialWarning {
        let (settings, workspace) = (self.settings.clone(), workspace.clone());
        ForeignCredentialWarning::new(move || {
            settings
                .as_deref()
                .is_none_or(|settings| crate::logins::foreign_warning_enabled(settings, &workspace))
        })
    }

    /// Makes SSH host keys at each start (and serves Git over SSH with them), or not.
    pub(crate) fn with_git_ssh(mut self, on: bool) -> Self {
        self.git_ssh = on;
        self
    }

    /// Keeps each workspace's login marker in `dir`.
    pub(crate) fn with_login_markers(mut self, dir: PathBuf) -> Self {
        self.login_keeper = self.login_keeper.with_markers(dir);
        self
    }

    /// The same with another limit on how long the credential store may take (tests).
    #[cfg(test)]
    fn with_vault_timeout(mut self, timeout: Duration) -> Self {
        self.vault_timeout = timeout;
        self
    }

    /// `workspace`'s environment lock.
    fn environment_lock(&self, workspace: &WorkspaceId) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self
            .environment
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(locks.entry(workspace.clone()).or_default())
    }

    /// Reads `workspace`'s environment off the async threads, giving up when the credential store
    /// does not answer in time.
    async fn environment_of(&self, workspace: &WorkspaceId) -> Result<Resolved, String> {
        let (store, vault, name) = (
            Arc::clone(&self.inputs.store),
            Arc::clone(&self.vault),
            workspace.clone(),
        );
        let read = tokio::task::spawn_blocking(move || resolve(&store, vault.as_ref(), &name));
        match tokio::time::timeout(self.vault_timeout, read).await {
            Ok(done) => done.map_err(|e| format!("cannot read {workspace}'s environment: {e}"))?,
            Err(_) => Err(format!(
                "the operating system's credential store did not answer within {} seconds while \
                 reading {workspace}'s secrets; unlock it or check that it is running, then try again",
                self.vault_timeout.as_secs().max(1)
            )),
        }
    }

    fn running(&self) -> std::sync::MutexGuard<'_, BTreeMap<WorkspaceId, Running>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The workspace's Git settings, from the database.
    pub(crate) fn git(&self, workspace: &WorkspaceId) -> Result<WorkspaceGit, String> {
        self.inputs
            .store
            .workspace_git(workspace)
            .map_err(|e| format!("cannot read {workspace}'s Git settings: {e}"))
    }

    /// Makes `workspace`'s CA for this start, reads its environment and the logins kept for it,
    /// and registers what it decrypts. A CA from an earlier start is replaced. With `capture` off
    /// the workspace's logins stay in the workspace and none of their hosts is decrypted.
    ///
    /// # Errors
    /// A message for the user: the settings or the environment cannot be read (a secret's value
    /// is missing from the credential store, say), or the CA cannot be made.
    pub(crate) async fn begin(
        &self,
        workspace: &WorkspaceId,
        capture: bool,
    ) -> Result<Began, String> {
        // Held until the workspace is registered: a change to its environment in the meantime
        // waits for that and then finds it running.
        let lock = self.environment_lock(workspace);
        let _environment = lock.lock().await;
        let resolved = self.environment_of(workspace).await?;
        // The registry is made here so that the logins kept for the workspace are in it before
        // the guest boots: its disk still holds the stand-ins its tools were given.
        let stand_ins = Arc::new(StandIns::new());
        let logins = self
            .login_keeper
            .logins(workspace, capture, Arc::clone(&stand_ins));
        logins.load().await;
        let ssh_keys = self.host_keys_for(workspace).await?;
        self.register(workspace, resolved, stand_ins, logins, ssh_keys)
    }

    /// This start's SSH host keys, when Git over SSH is on. The RSA key takes a moment, so it is
    /// made off the async threads.
    async fn host_keys_for(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<Option<Arc<GitSshHostKeys>>, String> {
        if !self.git_ssh {
            return Ok(None);
        }
        let made = tokio::task::spawn_blocking(GitSshHostKeys::generate)
            .await
            .map_err(|e| format!("cannot make {workspace}'s SSH host keys: {e}"))?;
        made.map(|keys| Some(Arc::new(keys)))
            .map_err(|e| format!("{workspace}: {e}"))
    }

    fn register(
        &self,
        workspace: &WorkspaceId,
        resolved: Resolved,
        stand_ins: Arc<StandIns>,
        logins: Logins,
        ssh_keys: Option<Arc<GitSshHostKeys>>,
    ) -> Result<Began, String> {
        let _one_at_a_time = self.syncing.lock().unwrap_or_else(PoisonError::into_inner);
        let git = self.git(workspace)?;
        // The common name shows in the guest's trust store and in a tool's error message.
        let name: String = format!("Puddle CA for {workspace}")
            .chars()
            .take(64)
            .collect();
        let ca = Arc::new(
            CaBuilder::new(&name)
                .build()
                .map_err(|e| format!("cannot make {workspace}'s certificate authority: {e}"))?,
        );
        let certificate = ca.certificate().clone();
        let injector = self.factory.as_ref().map_or_else(
            || git_injector(&self.inputs, workspace),
            |make| make(&self.inputs, workspace),
        );
        let Resolved { guest, entries } = resolved;
        stand_ins
            .replace_origin(StandInOrigin::Secret, entries)
            .map_err(|e| format!("the secrets of {workspace} cannot be registered: {e}"))?;
        // The registry and the list of running CAs change together, under the list's lock, so a
        // `resync` or an `end` never sees one without the other.
        let mut running = self.running();
        let now_keys = ssh_keys.clone();
        let now = Running {
            ca,
            ssh_keys,
            injector,
            stand_ins,
            logins,
            learned: Arc::new(self.learned_list(workspace)),
            foreign_warning: Arc::new(self.foreign_warning(workspace)),
        };
        self.terminations.insert(
            workspace.clone(),
            now.termination(
                &git,
                decrypted(&git, &now.stand_ins, &now.logins),
                self.interception(workspace, &now.learned)?,
                self.request_rules(workspace)?,
            ),
        );
        running.insert(workspace.clone(), now);
        let ssh = now_keys.map(|keys| SshListing::for_git(keys, &git));
        drop(running);
        Ok(Began {
            certificate,
            git,
            ssh,
            env: guest,
        })
    }

    /// Brings the running `workspace`'s secrets up to date with the store: a new secret's stand-in
    /// is registered and its hosts join what the workspace decrypts, a changed value or host list
    /// replaces the old, a removed secret's stand-in stops being swapped. All of it applies from
    /// the next request, with no restart. The guest's own environment is read at its next start; a
    /// login shell opened later sees the change through the file the caller rewrites.
    ///
    /// A workspace that does not run reads its environment at its next start. A failure leaves
    /// what the workspace had, is logged and reaches the user as [`Event::WorkspaceEnvFailed`]: the
    /// next change tries again.
    ///
    /// Returns the guest's variables as they are now (plain ones, a secret as its stand-in) when
    /// the change applied, so the caller can bring the running guest's login-shell file in line.
    pub(crate) async fn environment_changed(&self, workspace: &WorkspaceId) -> Option<GuestEnv> {
        let lock = self.environment_lock(workspace);
        let _environment = lock.lock().await;
        if !self.running().contains_key(workspace) {
            return None;
        }
        let applied = match self.environment_of(workspace).await {
            Ok(resolved) => {
                let guest = resolved.guest.clone();
                self.apply_environment(workspace, resolved).map(|()| guest)
            }
            Err(reason) => Err(reason),
        };
        match applied {
            Ok(guest) => Some(guest),
            Err(reason) => {
                self.failed(workspace, reason);
                None
            }
        }
    }

    /// Tells the user, and the log, that a change to `workspace`'s environment did not apply.
    fn failed(&self, workspace: &WorkspaceId, reason: String) {
        tracing::warn!(workspace = %workspace, %reason, "a change to the workspace's environment is not applied");
        self.inputs.events.emit(Event::WorkspaceEnvFailed {
            workspace: workspace.clone(),
            reason,
        });
    }

    /// Makes the registry of the running `workspace` hold `resolved`'s entries, then makes what it
    /// decrypts follow the registry. Not an error when the workspace stopped meanwhile: its next
    /// start reads its secrets itself.
    fn apply_environment(&self, workspace: &WorkspaceId, resolved: Resolved) -> Result<(), String> {
        let registry = self
            .running()
            .get(workspace)
            .map(|r| Arc::clone(&r.stand_ins));
        registry.map_or(Ok(()), |stand_ins| {
            stand_ins
                .replace_origin(StandInOrigin::Secret, resolved.entries)
                .map_err(|e| format!("the secrets of {workspace} cannot be registered: {e}"))?;
            self.resync(workspace).map(|_| ())
        })
    }

    /// Reads the running `workspace`'s settings again and changes what it decrypts to match, with
    /// the CA it has. `Ok(None)` when it has no CA (it is not running, so the next start reads the
    /// settings itself); the settings otherwise.
    pub(crate) fn resync(&self, workspace: &WorkspaceId) -> Result<Option<WorkspaceGit>, String> {
        let _one_at_a_time = self.syncing.lock().unwrap_or_else(PoisonError::into_inner);
        let git = self.git(workspace)?;
        // Held until the registry is changed: an `end` in between would otherwise leave a CA
        // registered for a sandbox that is gone.
        let running = self.running();
        let Some(now) = running.get(workspace).cloned() else {
            return Ok(None);
        };
        self.terminations.insert(
            workspace.clone(),
            now.termination(
                &git,
                decrypted(&git, &now.stand_ins, &now.logins),
                self.interception(workspace, &now.learned)?,
                self.request_rules(workspace)?,
            ),
        );
        drop(running);
        Ok(Some(git))
    }

    /// An empty learned list for `workspace` that writes each host it learns to the history in
    /// the store and tells the user, with how many times that host has now been refused.
    fn learned_list(&self, workspace: &WorkspaceId) -> PassThrough {
        let events = Arc::clone(&self.inputs.events);
        let store = Arc::clone(&self.inputs.store);
        let workspace = workspace.clone();
        PassThrough::new().observed(move |host| {
            let name = host.to_string();
            let count = match store.record_rejection(&workspace, &name) {
                Ok(row) => row.count,
                Err(e) => {
                    // The host is passed through all the same; only its history is lost.
                    tracing::warn!(%workspace, host = %name, error = %e, "cannot record the refused host");
                    1
                }
            };
            events.emit(Event::PassThroughLearned {
                workspace: workspace.clone(),
                host: name,
                count,
            });
        })
    }

    /// How `workspace` decrypts beyond its credential hosts, read from the store: its switch and
    /// the never-decrypt names (its own and the global ones). A name that does not parse is left
    /// out and logged; the settings API refuses such names, so this only meets old rows.
    fn interception(
        &self,
        workspace: &WorkspaceId,
        learned: &Arc<PassThrough>,
    ) -> Result<Interception, String> {
        let store = &self.inputs.store;
        let own = store
            .workspace_interception(workspace)
            .map_err(|e| format!("cannot read what {workspace} decrypts: {e}"))?;
        let global = store
            .global_never_decrypt()
            .map_err(|e| format!("cannot read the never-decrypt names: {e}"))?;
        let limits = store
            .learned_limits()
            .map_err(|e| format!("cannot read the learned-host limits: {e}"))?;
        learned.set_limits(
            Duration::from_secs(u64::from(limits.ttl_secs)),
            usize::try_from(limits.cap).unwrap_or(usize::MAX),
        );
        let settings = store
            .interception_settings()
            .map_err(|e| format!("cannot read the interception setting: {e}"))?;
        let mode = own.mode.unwrap_or_else(|| settings.effective_mode());
        let mut curated = TerminationSet::new();
        if mode == InterceptionMode::AllExceptCurated {
            // The list is built in and checked by a test; a bad entry is a bug, not user input.
            if let Ok(set) = TerminationSet::parse(CURATED_PASS_THROUGH.iter().copied()) {
                curated = set;
            }
        }
        let mut never = TerminationSet::new();
        for pattern in own.never_decrypt.iter().chain(&global) {
            match TerminationSet::parse([pattern.as_str()]) {
                Ok(one) => never.extend(&one),
                Err(e) => tracing::warn!(%pattern, error = %e, "a never-decrypt name is not valid"),
            }
        }
        Ok(Interception::new(Arc::clone(learned))
            .decrypt_all(mode.decrypts_unknown_hosts())
            .curated(curated)
            .learn_from_rejections(settings.learn_from_rejections)
            .never_decrypt(never))
    }

    /// The rules on method and path that apply to `workspace`, global and its own. A row the
    /// proxy cannot take (a host that is no longer valid) is left out and logged.
    fn request_rules(&self, workspace: &WorkspaceId) -> Result<RequestRules, String> {
        let rows = self
            .inputs
            .store
            .request_rules(Some(workspace))
            .map_err(|e| format!("cannot read the rules on method and path: {e}"))?;
        let mut rules = Vec::with_capacity(rows.len());
        for row in rows {
            match RequestRule::new(RuleId(row.id), &row.host, row.methods, row.path) {
                Ok(rule) => rules.push(rule),
                Err(e) => {
                    tracing::warn!(id = row.id, host = %row.host, error = %e, "a rule on method and path is not valid");
                }
            }
        }
        Ok(RequestRules::new(rules))
    }

    /// Drops `workspace`'s CA and what it decrypts. Connections already open keep what they have;
    /// the key is gone with the last of them.
    pub(crate) fn end(&self, workspace: &WorkspaceId) {
        let mut running = self.running();
        self.terminations.remove(workspace);
        running.remove(workspace);
    }

    /// Forgets the login of `service` (its short name or the name the user sees) kept for
    /// `workspace`: its tokens leave the credential store and, in a running workspace, its
    /// stand-ins stop being swapped. `Ok(false)` for a service Puddle has no login profile for.
    /// A workspace that is not running gets a throwaway [`Logins`] over the same store and marker,
    /// so a notice about a login kept earlier ends the same way.
    pub(crate) async fn forget_login(
        &self,
        workspace: &WorkspaceId,
        service: &str,
    ) -> Result<bool, puddle_logins::ForgetError> {
        let running = self.running().get(workspace).map(|r| r.logins.clone());
        let logins = running.unwrap_or_else(|| {
            self.login_keeper
                .logins(workspace, false, Arc::new(StandIns::new()))
        });
        logins.forget(service).await
    }

    /// The logins kept for `workspace`, for the user to look at. A workspace that is not running
    /// gets a throwaway [`Logins`] over the same store and marker, as [`Self::forget_login`] does.
    pub(crate) async fn kept_logins(&self, workspace: &WorkspaceId) -> puddle_logins::KeptList {
        let running = self.running().get(workspace).map(|r| r.logins.clone());
        let logins = running.unwrap_or_else(|| {
            self.login_keeper
                .logins(workspace, false, Arc::new(StandIns::new()))
        });
        logins.kept_list().await
    }

    /// What `workspace` decrypts and the CA that certifies it, while it runs.
    pub(crate) fn termination(&self, workspace: &WorkspaceId) -> Option<Arc<Termination>> {
        self.terminations.termination(workspace)
    }

    /// Removes what a deleted `workspace` held: its own variables and stand-ins in the store and
    /// its own secrets' values in the credential store. The workspace is gone either way, so a
    /// failure is logged and shown ([`Event::WorkspaceEnvFailed`]) instead of stopping anything: a
    /// value that stays behind is unreachable (nothing names it any more) but is still the user's.
    pub(crate) async fn forget(&self, workspace: &WorkspaceId) {
        let (store, vault, name) = (
            Arc::clone(&self.inputs.store),
            Arc::clone(&self.vault),
            workspace.clone(),
        );
        let outcome = tokio::task::spawn_blocking(move || {
            let deletion = store
                .delete_workspace(&name)
                .map_err(|e| format!("cannot remove {name}'s rules and environment: {e}"))?;
            let left: Vec<String> = deletion
                .secrets
                .iter()
                .filter(|(_, id)| vault.delete(id).is_err())
                .map(|(var, _)| var.to_string())
                .collect();
            if left.is_empty() {
                return Ok(());
            }
            Err(format!(
                "the stored value of {} could not be removed from the operating system's credential \
                 store; remove it there (it is filed under Puddle)",
                left.join(", ")
            ))
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|removed| removed);
        if let Err(reason) = outcome {
            self.failed(workspace, reason);
        }
        self.login_keeper.delete_workspace(workspace).await;
    }

    /// The workspaces that have a CA now.
    pub(crate) fn running_workspaces(&self) -> Vec<WorkspaceId> {
        self.running().keys().cloned().collect()
    }
}

/// The log of what the last read of each credential came to, which sends
/// [`Event::IdentitiesChanged`] when a credential's outcome moves so the screens refetch the
/// identities and show it. Memory only: after a restart a credential has no entry until it is read.
pub(crate) fn credential_read_log(sink: Arc<dyn EventSink>) -> Arc<ReadLog> {
    Arc::new(ReadLog::new().with_on_change(move || sink.emit(Event::IdentitiesChanged {})))
}

/// One sign-in notice for a source is sent at most this often, however many reads fail (a Git
/// client retries).
const SIGN_IN_NOTICE_EVERY: Duration = Duration::from_secs(10);

/// Most sources remembered as lately noticed.
const MAX_SIGN_IN_SOURCES: usize = 256;

/// Sends [`Event::CredentialSignInNeeded`] for a read of a source that needs the user to sign in,
/// at most once per source in [`SIGN_IN_NOTICE_EVERY`], until the returned task is aborted. The
/// sign-in itself starts only on the user's click.
pub(crate) fn forward_sign_in_needed<F: Fetch + 'static>(
    secrets: &Arc<SecretCache<F>>,
    sink: Arc<dyn EventSink>,
) -> JoinHandle<()> {
    let mut notices = secrets.subscribe();
    tokio::spawn(async move {
        let mut last: HashMap<SourceSpec, Instant> = HashMap::new();
        loop {
            match notices.recv().await {
                Ok(notice) => {
                    let now = Instant::now();
                    if last.len() >= MAX_SIGN_IN_SOURCES {
                        last.retain(|_, at| now.duration_since(*at) < SIGN_IN_NOTICE_EVERY);
                    }
                    let recent = last
                        .get(&notice.source)
                        .is_some_and(|at| now.duration_since(*at) < SIGN_IN_NOTICE_EVERY);
                    if recent {
                        continue;
                    }
                    // A flood of different sources past the cap is not a flood of events.
                    if last.len() >= MAX_SIGN_IN_SOURCES {
                        tracing::warn!(source = %notice.source.describe(), "too many sources need a sign-in at once; this notice is not shown");
                        continue;
                    }
                    sink.emit(Event::CredentialSignInNeeded {
                        host: notice.source.scope().host.to_string(),
                        source: notice.source.describe(),
                    });
                    last.insert(notice.source, now);
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

#[cfg(test)]
mod tests;
