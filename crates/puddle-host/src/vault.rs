// SPDX-License-Identifier: GPL-3.0-or-later
//! The credential store Puddle keeps its values in, and the clean-up of what nothing names.

use std::sync::Arc;

use puddle_secrets::{ChunkedStore, IndexedStore, KeyringStore, SecretStore, StoredId, Sweep};
use puddle_store::Store;
use puddle_types::{Problem, Problems};

use crate::proxy_passwords::ID_PREFIX;

/// The operating system's credential store, with long values split over entries and an index of
/// the ids kept, so [`sweep_in_background`] can find a value its row never named.
pub(crate) fn os_vault() -> Arc<dyn SecretStore> {
    Arc::new(IndexedStore::new(ChunkedStore::new(KeyringStore::new())))
}

/// The problem that stands while the clean-up cannot finish; the next sweep that does ends it.
const SWEEP_PROBLEM: &str = "credential-store-sweep";

/// Removes every value in `vault` that no identity and no environment secret names. `Err` says why
/// nothing was removed: the rows or the store could not be read.
fn sweep(vault: &dyn SecretStore, store: &Store) -> Result<Sweep, &'static str> {
    let in_use = store.secret_ids_in_use().map_err(|err| {
        tracing::warn!(%err, "credential store sweep skipped: the rows could not be read");
        "Puddle could not read which tokens and secrets it still uses"
    })?;
    keep_local(vault, in_use.iter());
    vault
        .sweep_orphans(&|id| in_use.contains(id) || id.as_str().starts_with(ID_PREFIX))
        .map_err(|_| {
            tracing::warn!("credential store sweep skipped: the store did not answer");
            "the operating system's credential store did not answer"
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
/// starting must not wait for it. Values this run writes meanwhile are never swept.
///
/// The sweep holds the store, so the host awaits the returned handle at shutdown: a restart on
/// the same folder must not open the database while this run's connection is still closing.
pub(crate) fn sweep_in_background(
    vault: Arc<dyn SecretStore>,
    store: Arc<Store>,
    problems: Arc<Problems>,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || report(sweep(vault.as_ref(), &store), &problems))
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
