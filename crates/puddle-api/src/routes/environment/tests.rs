// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::{MemoryStore, Secret};
use puddle_store::{Limits, ManualClock};

use super::*;

fn entry_of(store: &Store, name: &str, draft: EnvDraft) -> EnvEntry {
    store
        .set_env(
            UserId::OWNER,
            &EnvScope::Global,
            &EnvName::new(name).unwrap(),
            draft,
        )
        .unwrap()
        .entry
}

#[test]
fn the_value_of_a_replaced_secret_is_removed_unless_the_new_entry_keeps_it() {
    let store = Store::open_in_memory(Arc::new(ManualClock::new(1)), Limits::default()).unwrap();
    let memory = Arc::new(MemoryStore::new());
    let vault: Arc<dyn SecretStore> = memory.clone();
    let id = |text: &str| StoredId::new(text).unwrap();
    let hosts = || SecretHost::list(&["a.example.com"]).unwrap();
    for name in ["env-a", "env-b"] {
        memory.set(&id(name), &Secret::new("v".to_owned())).unwrap();
    }
    let secret_a = entry_of(&store, "A", EnvDraft::secret(id("env-a"), hosts()).unwrap());
    let plain = entry_of(&store, "P", EnvDraft::plain("x").unwrap());

    // Kept: the same id stays. Another request's secret replaced it: the other id goes.
    drop_replaced(&vault, Some(&secret_a), Some(&id("env-a")));
    assert_eq!(memory.ids(), ["env-a", "env-b"]);
    drop_replaced(&vault, Some(&secret_a), Some(&id("env-b")));
    assert_eq!(memory.ids(), ["env-b"]);
    // Nothing replaced, or a plain variable replaced: nothing to remove.
    drop_replaced(&vault, None, Some(&id("env-b")));
    drop_replaced(&vault, Some(&plain), None);
    assert_eq!(memory.ids(), ["env-b"]);
    // A secret that became plain has no value to keep.
    let secret_b = entry_of(&store, "B", EnvDraft::secret(id("env-b"), hosts()).unwrap());
    drop_replaced(&vault, Some(&secret_b), None);
    assert_eq!(memory.ids(), Vec::<String>::new());
    // A credential store that fails is logged, not an error.
    memory.break_it();
    drop_replaced(&vault, Some(&secret_a), None);
}
