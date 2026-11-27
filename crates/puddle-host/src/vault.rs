// SPDX-License-Identifier: AGPL-3.0-or-later
//! The credential store Puddle keeps its values in, and the clean-up of what nothing names.

use std::path::Path;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use puddle_secrets::{
    ChunkedStore, IndexedStore, KeyringStore, LoginSession, SecretStore, StoreHealth, StoreKind,
    StoredId, Sweep, probe_os_store, probe_store, reconnect_os_store, write_record,
};
use puddle_services::credential_store::CredentialStoreService;
use puddle_store::Store;
use puddle_types::{Problem, Problems, WorkspaceId};

use crate::proxy_passwords::ID_PREFIX;

mod admin;
mod choose;

pub use admin::{
    MigrateOutcome, ResetOutcome, StoreState, VaultAdminError, VaultStatus, vault_migrate,
    vault_reset, vault_status,
};
pub use choose::SecretSetup;
pub(crate) use choose::{Chosen, open};
use choose::{Probe, WriteWhenWorking};

/// The operating system's credential store, with long values split over entries and an index of
/// the ids kept, so [`sweep_in_background`] can find a value its row never named. The index is
/// this data folder's own: the store is one per OS user, and a host started with another
/// `--data-dir` must not sweep these values.
pub(crate) fn os_vault(data: &Path) -> Arc<dyn SecretStore> {
    scoped_vault(ChunkedStore::new(KeyringStore::new()), data)
}

fn scoped_vault(inner: impl SecretStore + 'static, data: &Path) -> Arc<dyn SecretStore> {
    Arc::new(IndexedStore::scoped(inner, &credential_scope(data)))
}

/// The file in a data folder that holds the name its credential-store index goes by.
const SCOPE_FILE: &str = "credential-scope";

/// The name that tells this data folder's credential-store entries from another folder's: a random
/// one kept in the folder, so it follows the folder when it is moved. A folder that cannot keep
/// one (read-only) falls back to a name made from its path, which is stable until the folder
/// moves; either way two folders never share a name.
fn credential_scope(data: &Path) -> String {
    let file = data.join(SCOPE_FILE);
    if let Ok(text) = std::fs::read_to_string(&file) {
        let text = text.trim();
        if text.len() == SCOPE_LEN && text.bytes().all(|b| b.is_ascii_hexdigit()) {
            return text.to_owned();
        }
    }
    let mut bytes = [0u8; SCOPE_LEN / 2];
    let fresh = getrandom::fill(&mut bytes).is_ok().then(|| hex(&bytes));
    if let Some(fresh) = fresh
        && puddle_fs::private::write_atomic(&file, fresh.as_bytes()).is_ok()
    {
        return fresh;
    }
    tracing::warn!("could not keep a credential store name in the data folder; using its path");
    let path = std::fs::canonicalize(data).unwrap_or_else(|_| data.to_path_buf());
    let digest = Sha256::digest(path.to_string_lossy().as_bytes());
    hex(&digest[..SCOPE_LEN / 2])
}

const SCOPE_LEN: usize = 16;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// The problem that stands while the clean-up cannot finish; the next sweep that does ends it.
const SWEEP_PROBLEM: &str = "credential-store-sweep";

/// Removes every value in `vault` that nothing names: no identity, no environment secret, no proxy
/// sign-in and no saved login of one of `workspaces` (the saved logins of a deleted workspace go).
/// `Err` says why nothing was removed: the rows or the store could not be read.
fn sweep(
    vault: &dyn SecretStore,
    store: &Store,
    workspaces: &[WorkspaceId],
) -> Result<Sweep, &'static str> {
    let in_use = store.secret_ids_in_use().map_err(|err| {
        tracing::warn!(%err, "credential store sweep skipped: the rows could not be read");
        "Puddle could not read which tokens and secrets it still uses"
    })?;
    keep_local(vault, in_use.iter());
    vault
        .sweep_orphans(&|id| {
            in_use.contains(id)
                || id.as_str().starts_with(ID_PREFIX)
                || workspaces
                    .iter()
                    .any(|workspace| puddle_logins::is_saved_login_of(id, workspace))
        })
        .map_err(|_| {
            tracing::warn!("credential store sweep skipped: the store did not answer");
            "the credential store did not answer"
        })
}

/// Rewrites the values an earlier version stored so that they stay on this computer (a Windows
/// roaming profile would copy them otherwise): everything the store lists, plus what a row
/// names, which covers values written before the store kept a list. Nothing here is the user's
/// to act on, so a failure is only logged; the next start tries again.
fn keep_local<'a>(vault: &dyn SecretStore, in_use: impl Iterator<Item = &'a StoredId>) {
    let mut failed = vault.keep_all_local().unwrap_or_else(|_| {
        tracing::warn!("could not list the credential store entries to keep on this computer");
        0
    });
    failed += in_use.filter(|id| vault.keep_local(id).is_err()).count();
    if failed > 0 {
        tracing::warn!(
            failed,
            "credential store entries could not be kept on this computer"
        );
    }
}

/// Tells the user what the sweep found: nothing when it removed what it should, a problem when a
/// value it meant to remove is still there or it could not run at all.
fn report(outcome: Result<Sweep, &'static str>, problems: &Problems) {
    match outcome {
        Ok(Sweep { removed, stuck: 0 }) => {
            if removed > 0 {
                tracing::info!(
                    removed,
                    "removed values nothing refers to from the credential store"
                );
            }
            problems.clear(SWEEP_PROBLEM);
        }
        Ok(Sweep { removed, stuck }) => {
            tracing::info!(removed, stuck, "credential store sweep left values behind");
            problems.raise(Problem::new(
                SWEEP_PROBLEM,
                format!(
                    "Puddle could not remove {stuck} leftover {} from the credential store",
                    if stuck == 1 { "value" } else { "values" }
                ),
                "They belong to nothing in Puddle any more, for example a token you pasted and \
                 never saved, or one left by a delete. Unlock the credential store if it is locked; Puddle tries again the \
                 next time it starts.",
            ));
        }
        Err(why) => problems.raise(Problem::new(
            SWEEP_PROBLEM,
            "Puddle could not clean up the credential store",
            format!(
                "{why}, so tokens left over from a delete or a pasted token you never saved stay \
                 where they are. Puddle tries again the next time it starts."
            ),
        )),
    }
}

/// Runs [`sweep`] off the async threads: a locked keyring may take a long time to answer, and
/// starting must not wait for it. Values this run writes meanwhile are never swept, so a workspace
/// made after the start keeps its saved logins too.
///
/// The sweep holds the store, so the host awaits the returned handle at shutdown: a restart on
/// the same folder must not open the database while this run's connection is still closing.
pub(crate) fn sweep_in_background(
    vault: Arc<dyn SecretStore>,
    store: Arc<Store>,
    workspaces: Vec<WorkspaceId>,
    problems: Arc<Problems>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        report(sweep(vault.as_ref(), &store, &workspaces), &problems);
    })
}

/// The problem that stands while the credential store refuses a write. A store that failed once
/// stays failed for the life of the process, so only a restart ends it.
const STORE_PROBLEM: &str = "credential-store";

/// Raises the one problem that names why `health` is not [`StoreHealth::Working`], worded for
/// Linux when `linux` is set; a working store raises none.
fn report_health(health: StoreHealth, linux: bool, problems: &Problems) {
    match health.advice(linux) {
        None => problems.clear(STORE_PROBLEM),
        Some(advice) => problems.raise(Problem::new(STORE_PROBLEM, advice.title, advice.fix)),
    }
}

/// The problem for a file vault that does not take a write: unlike the operating system's store
/// it has no lock and no service, so the usual cause is the disk.
fn report_file_health(health: StoreHealth, problems: &Problems) {
    if health == StoreHealth::Working {
        problems.clear(STORE_PROBLEM);
    } else {
        problems.raise(Problem::new(
            STORE_PROBLEM,
            "Puddle cannot save secrets: its file vault does not take a write",
            "The vault opened but a test write failed. Check the free space and the permissions of the `secrets` folder in the data folder, then restart Puddle.",
        ));
    }
}

impl Chosen {
    /// Writes and deletes a throwaway value in the store this start chose, off the async threads
    /// (a locked keyring may be slow to answer), and raises a problem naming the cause when the
    /// write fails. It writes the decision down once the operating system's store works, and
    /// logs the headless warning.
    pub(crate) fn check_in_background(
        &self,
        problems: Arc<Problems>,
    ) -> tokio::task::JoinHandle<()> {
        run_check(
            self.probe.clone(),
            self.record_when_working.clone(),
            self.warn_without_flag,
            problems,
        )
    }
}

/// Whether a start without the headless flag deserves the warning: it has no graphical login
/// session, or its credential store does not take a write.
fn warns(session: LoginSession, health: StoreHealth) -> bool {
    session == LoginSession::NotGraphical || health != StoreHealth::Working
}

/// Logs the advice to start with `--headless` when `session` and `health` say the operating
/// system's store may not be there for the secrets.
fn warn_without_headless_flag(session: LoginSession, health: StoreHealth) {
    if !warns(session, health) {
        return;
    }
    let graphical_session = session != LoginSession::NotGraphical;
    let store_works = health == StoreHealth::Working;
    let advice = headless_advice(puddle_doctor::SERVE_BUILT_IN);
    tracing::warn!(graphical_session, store_works, "{advice}");
}

/// The warning's text. A build without the `puddle serve` command (`serve`) cannot be told to
/// start headless, so its advice names only the keyring.
fn headless_advice(serve: bool) -> &'static str {
    if serve {
        "Puddle was started without --headless and keeps its secrets in the operating \
         system's credential store, which has no unlocked keyring to rely on here. On a \
         server start `puddle serve --headless` (or set PUDDLE_HEADLESS=1) so secrets go \
         in Puddle's own file vault; that takes effect only for a data folder that has \
         not recorded a store yet (`secrets/store.json`)"
    } else {
        "Puddle keeps its secrets in the operating system's credential store, which has \
         no unlocked keyring to rely on here. Unlock the keyring after every reboot"
    }
}

fn run_check(
    probe: Probe,
    record_when_working: Option<WriteWhenWorking>,
    warn_without_flag: Option<LoginSession>,
    problems: Arc<Problems>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let Some(health) = check_now(&probe, false, &problems) else {
            return;
        };
        if health == StoreHealth::Working
            && let Some(decision) = record_when_working
            && write_record(&decision.data, StoreKind::Os, decision.rule).is_err()
        {
            tracing::warn!(
                "could not write which store keeps the secrets; the next start tries again"
            );
        }
        if let Some(session) = warn_without_flag {
            warn_without_headless_flag(session, health);
        }
    })
}

/// One check of the chosen store: probes it and raises or clears the problem to match. `None`
/// when there is nothing to probe. `reconnect` connects to the operating system's store afresh
/// first, which is what lets a Puddle that began with a locked or missing store use it once the
/// user has unlocked it.
fn check_now(probe: &Probe, reconnect: bool, problems: &Problems) -> Option<StoreHealth> {
    let health = match probe {
        Probe::Store(store) => probe_store(store.as_ref(), || true),
        Probe::Os => {
            if reconnect {
                // A refused reconnect shows in the probe below as the failure it is.
                let _ = reconnect_os_store();
            }
            probe_os_store()
        }
        Probe::File(store) => {
            let health = probe_store(store.as_ref(), || true);
            report_file_health(health, problems);
            return Some(health);
        }
        Probe::Skip => return None,
    };
    report_health(health, cfg!(target_os = "linux"), problems);
    Some(health)
}

/// The check behind the first-run banner's Retry.
pub(crate) struct StoreRetry {
    probe: Probe,
    problems: Arc<Problems>,
}

impl Chosen {
    /// The check the first-run banner's Retry runs against the store this start chose.
    pub(crate) fn retry_check(&self, problems: Arc<Problems>) -> StoreRetry {
        StoreRetry {
            probe: self.probe.clone(),
            problems,
        }
    }
}

impl CredentialStoreService for StoreRetry {
    fn retry(&self) -> Option<bool> {
        check_now(&self.probe, true, &self.problems).map(|health| health == StoreHealth::Working)
    }
}

/// How long shutdown waits for the sweep: a locked keyring may take far longer, and a quit must
/// not hang on it. The sweep goes on in the background; the next start repeats whatever it left.
pub(crate) const SWEEP_SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// How the wait for the sweep at shutdown ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SweepEnd {
    Finished,
    /// The sweep task panicked (a panic inside the sweep is not reported anywhere else).
    Panicked,
    /// Still running after the wait.
    StillRunning,
}

/// Waits up to `within` for the sweep started by [`sweep_in_background`], and logs a panic or a
/// sweep that is still running.
pub(crate) async fn finish_sweep(
    sweep: tokio::task::JoinHandle<()>,
    within: std::time::Duration,
) -> SweepEnd {
    let end = match tokio::time::timeout(within, sweep).await {
        Ok(Ok(())) => SweepEnd::Finished,
        Ok(Err(_)) => SweepEnd::Panicked,
        Err(_) => SweepEnd::StillRunning,
    };
    match end {
        SweepEnd::Finished => {}
        SweepEnd::Panicked => {
            tracing::warn!("the credential store sweep panicked; the next start repeats it");
        }
        SweepEnd::StillRunning => {
            tracing::warn!(
                waited = ?within,
                "the credential store did not answer while quitting; the sweep is left running"
            );
        }
    }
    end
}

#[cfg(test)]
mod tests;
