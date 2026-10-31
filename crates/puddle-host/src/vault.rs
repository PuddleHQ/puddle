// SPDX-License-Identifier: GPL-3.0-or-later
//! The credential store Puddle keeps its values in, and the clean-up of what nothing names.

use std::sync::Arc;

use puddle_secrets::{ChunkedStore, IndexedStore, KeyringStore, SecretStore, Sweep};
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
    vault
        .sweep_orphans(&|id| in_use.contains(id) || id.as_str().starts_with(ID_PREFIX))
        .map_err(|_| {
            tracing::warn!("credential store sweep skipped: the store did not answer");
            "the operating system's credential store did not answer"
        })
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
pub(crate) fn sweep_in_background(
    vault: Arc<dyn SecretStore>,
    store: Arc<Store>,
    problems: Arc<Problems>,
) {
    tokio::task::spawn_blocking(move || report(sweep(vault.as_ref(), &store), &problems));
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
        assert_eq!(
            sweep(&later, &store),
            Ok(Sweep {
                removed: 2,
                stuck: 0
            })
        );
        assert!(later.get(&id("env-named")).unwrap().is_some());
        assert!(later.get(&id("env-orphan")).unwrap().is_none());
        assert!(later.get(&id("tok-orphan")).unwrap().is_none());
    }

    #[test]
    fn a_sweep_keeps_the_proxy_sign_ins_which_no_row_names() {
        let store =
            Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
        let disk = Arc::new(MemoryStore::new());
        // An earlier run wrote a sign-in and an orphan; this run sweeps.
        let earlier = Arc::new(IndexedStore::new(Shared(disk.clone())));
        let passwords = crate::ProxyPasswords::new(earlier.clone());
        let proxy = puddle_upstream::ProxyAddr::new("proxy.corp", 8080);
        passwords
            .set(&proxy, "alice", &puddle_types::fixture::password("vault"))
            .unwrap();
        put(earlier.as_ref(), "tok-orphan");
        let later = Arc::new(IndexedStore::new(Shared(disk)));
        assert_eq!(
            sweep(later.as_ref(), &store),
            Ok(Sweep {
                removed: 1,
                stuck: 0
            })
        );
        let again = crate::ProxyPasswords::new(later);
        assert_eq!(again.load().unwrap(), 1);
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
            Err("the operating system's credential store did not answer")
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
        sweep_in_background(
            Arc::new(IndexedStore::new(Shared(disk.clone()))),
            store,
            Arc::new(Problems::new(Arc::new(puddle_types::NullSink))),
        );
        puddle_test_kit::eventually("the sweep ran", std::time::Duration::from_secs(5), || {
            disk.get(&id("tok-a")).unwrap().is_none()
        })
        .await;
    }

    /// A store that will not delete one id.
    struct Stuck(Arc<MemoryStore>);

    impl SecretStore for Stuck {
        fn get(&self, id: &StoredId) -> Result<Option<Secret>, puddle_secrets::StoreError> {
            self.0.get(id)
        }
        fn set(&self, id: &StoredId, s: &Secret) -> Result<(), puddle_secrets::StoreError> {
            self.0.set(id, s)
        }
        fn delete(&self, id: &StoredId) -> Result<(), puddle_secrets::StoreError> {
            if id.as_str() == "tok-stuck" {
                return Err(puddle_secrets::StoreError);
            }
            self.0.delete(id)
        }
    }

    fn problems() -> Problems {
        Problems::new(Arc::new(puddle_types::NullSink))
    }

    #[test]
    fn a_sweep_that_leaves_a_value_behind_raises_a_problem_until_a_later_sweep_removes_it() {
        let store =
            Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
        let disk = Arc::new(MemoryStore::new());
        let earlier = IndexedStore::new(Shared(disk.clone()));
        put(&earlier, "tok-stuck");
        put(&earlier, "tok-gone");
        let list = problems();

        report(
            sweep(&IndexedStore::new(Stuck(disk.clone())), &store),
            &list,
        );
        let raised = list.list();
        assert_eq!(raised.len(), 1, "{raised:?}");
        assert_eq!(raised[0].key, SWEEP_PROBLEM);
        assert_eq!(
            raised[0].title,
            "Puddle could not remove 1 leftover value from the credential store"
        );
        assert!(disk.get(&id("tok-gone")).unwrap().is_none());

        // The store lets it go on the next start: the problem ends by itself.
        report(
            sweep(&IndexedStore::new(Shared(disk.clone())), &store),
            &list,
        );
        assert_eq!(list.list(), Vec::new());
        assert!(disk.get(&id("tok-stuck")).unwrap().is_none());
    }

    #[test]
    fn a_sweep_that_cannot_run_raises_a_problem_naming_why_and_a_clean_one_raises_none() {
        let store =
            Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
        let disk = Arc::new(MemoryStore::new());
        put(&IndexedStore::new(Shared(disk.clone())), "tok-a");
        let list = problems();

        disk.break_it();
        report(
            sweep(&IndexedStore::new(Shared(disk.clone())), &store),
            &list,
        );
        let raised = list.list();
        assert_eq!(raised.len(), 1);
        assert!(raised[0].detail.contains("credential store did not answer"));
        disk.heal();

        report(
            sweep(&IndexedStore::new(Shared(disk.clone())), &store),
            &list,
        );
        assert_eq!(list.list(), Vec::new());
        report(sweep(&IndexedStore::new(Shared(disk)), &store), &list);
        assert_eq!(list.list(), Vec::new());
    }
}
