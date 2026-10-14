// SPDX-License-Identifier: GPL-3.0-or-later
//! The credential store puddle keeps its values in, and the clean-up of what nothing names.

use std::sync::Arc;

use puddle_secrets::{ChunkedStore, IndexedStore, KeyringStore, SecretStore};
use puddle_store::Store;

/// The operating system's credential store, with long values split over entries and an index of
/// the ids kept, so [`sweep_in_background`] can find a value its row never named.
pub(crate) fn os_vault() -> Arc<dyn SecretStore> {
    Arc::new(IndexedStore::new(ChunkedStore::new(KeyringStore)))
}

/// Removes every value in `vault` that no identity and no environment secret names. Says how many
/// went; `None` when the rows or the store could not be read, in which case nothing is removed.
fn sweep(vault: &dyn SecretStore, store: &Store) -> Option<usize> {
    let in_use = store
        .secret_ids_in_use()
        .inspect_err(|err| tracing::warn!(%err, "credential store sweep skipped: the rows could not be read"))
        .ok()?;
    vault
        .sweep_orphans(&|id| in_use.contains(id))
        .inspect_err(|_| tracing::warn!("credential store sweep skipped: the store did not answer"))
        .ok()
}

/// Runs [`sweep`] off the async threads: a locked keyring may take a long time to answer, and
/// starting must not wait for it. Values this run writes meanwhile are never swept.
pub(crate) fn sweep_in_background(vault: Arc<dyn SecretStore>, store: Arc<Store>) {
    tokio::task::spawn_blocking(move || {
        if let Some(removed) = sweep(vault.as_ref(), &store)
            && removed > 0
        {
            tracing::info!(
                removed,
                "removed values nothing refers to from the credential store"
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use puddle_secrets::{MemoryStore, Secret, StoredId};
    use puddle_store::{EnvDraft, EnvName, EnvScope, Limits, ManualClock, SecretHost};

    use super::*;

    fn id(text: &str) -> StoredId {
        StoredId::new(text).unwrap()
    }

    fn put(vault: &dyn SecretStore, name: &str) {
        vault.set(&id(name), &Secret::new("value".into())).unwrap();
    }

    /// A shared memory store: one handle for the run that crashed, one for the run after it.
    struct Shared(Arc<MemoryStore>);

    impl SecretStore for Shared {
        fn get(&self, id: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
            self.0.get(id)
        }
        fn set(&self, id: &StoredId, s: &Secret) -> Result<(), puddle_secrets::StoreError> {
            self.0.set(id, s)
        }
        fn delete(&self, id: &StoredId) -> Result<(), puddle_secrets::StoreError> {
            self.0.delete(id)
        }
    }

    #[test]
    fn the_os_vault_is_built_without_touching_the_os_store() {
        let _vault = os_vault();
    }

    #[test]
    fn a_sweep_removes_what_no_identity_or_variable_names_and_keeps_the_rest() {
        let store =
            Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
        store
            .set_env(
                &EnvScope::Global,
                &EnvName::new("TOKEN").unwrap(),
                EnvDraft::secret(
                    id("env-named"),
                    vec![SecretHost::new("a.example.com").unwrap()],
                )
                .unwrap(),
            )
            .unwrap();
        let disk = Arc::new(MemoryStore::new());
        let earlier = IndexedStore::new(Shared(disk.clone()));
        for name in ["env-named", "env-orphan", "tok-orphan"] {
            put(&earlier, name);
        }
        let later = IndexedStore::new(Shared(disk.clone()));
        assert_eq!(sweep(&later, &store), Some(2));
        assert!(later.get(&id("env-named")).unwrap().is_some());
        assert!(later.get(&id("env-orphan")).unwrap().is_none());
        assert!(later.get(&id("tok-orphan")).unwrap().is_none());
    }

    #[test]
    fn a_store_that_does_not_answer_is_left_alone_and_the_sweep_says_so() {
        let store =
            Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
        let disk = Arc::new(MemoryStore::new());
        put(&IndexedStore::new(Shared(disk.clone())), "tok-a");
        disk.break_it();
        assert_eq!(
            sweep(&IndexedStore::new(Shared(disk.clone())), &store),
            None
        );
        disk.heal();
        assert!(disk.get(&id("tok-a")).unwrap().is_some());
    }

    #[tokio::test]
    async fn the_background_sweep_runs_off_the_async_threads() {
        let store = Arc::new(
            Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap(),
        );
        let disk = Arc::new(MemoryStore::new());
        put(&IndexedStore::new(Shared(disk.clone())), "tok-a");
        sweep_in_background(Arc::new(IndexedStore::new(Shared(disk.clone()))), store);
        for _ in 0..200 {
            if disk.get(&id("tok-a")).unwrap().is_none() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("the sweep did not run");
    }
}
