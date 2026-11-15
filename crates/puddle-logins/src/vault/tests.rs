// SPDX-License-Identifier: AGPL-3.0-or-later
use puddle_secrets::MemoryStore;

use super::*;
use crate::profile::{CLAUDE, GITHUB};

fn workspace(name: &str) -> WorkspaceId {
    WorkspaceId::new(name).unwrap()
}

fn vault(store: &Arc<MemoryStore>, name: &str) -> Vault {
    Vault::new(Arc::clone(store) as Arc<dyn SecretStore>, &workspace(name))
}

#[test]
fn a_pair_round_trips_and_a_missing_one_is_none() {
    let store = Arc::new(MemoryStore::new());
    let v = vault(&store, "alpha");
    assert!(v.read(&GITHUB, Role::Access).unwrap().is_none());
    v.write(&GITHUB, Role::Access, "gho_standin", "gho_CANARY-real")
        .unwrap();
    let saved = v.read(&GITHUB, Role::Access).unwrap().unwrap();
    assert_eq!(saved.stand_in, "gho_standin");
    assert_eq!(&*saved.real, "gho_CANARY-real");
    v.write(&GITHUB, Role::Access, "gho_standin", "gho_CANARY-newer")
        .unwrap();
    assert_eq!(
        &*v.read(&GITHUB, Role::Access).unwrap().unwrap().real,
        "gho_CANARY-newer"
    );
    v.delete(&GITHUB, Role::Access).unwrap();
    v.delete(&GITHUB, Role::Access).unwrap();
    assert!(v.read(&GITHUB, Role::Access).unwrap().is_none());
}

#[test]
fn each_workspace_profile_and_role_has_an_entry_of_its_own() {
    let store = Arc::new(MemoryStore::new());
    let (a, b) = (vault(&store, "alpha"), vault(&store, "beta"));
    a.write(&GITHUB, Role::Access, "s1", "r1").unwrap();
    assert!(b.read(&GITHUB, Role::Access).unwrap().is_none());
    assert!(a.read(&GITHUB, Role::Refresh).unwrap().is_none());
    assert!(a.read(&CLAUDE, Role::Access).unwrap().is_none());
    b.write(&GITHUB, Role::Access, "s2", "r2").unwrap();
    assert_eq!(
        a.read(&GITHUB, Role::Access).unwrap().unwrap().stand_in,
        "s1"
    );
}

#[test]
fn entry_names_fit_the_store_for_the_longest_workspace_name() {
    let long = "a".repeat(63);
    for profile in crate::profile::builtin() {
        for role in profile.roles() {
            let id = slot_id(&long, profile.id, role).unwrap();
            assert!(id.as_str().len() <= 64, "{id}");
            assert!(id.as_str().starts_with("login-"));
        }
    }
    assert_ne!(
        slot_id("alpha", "github", Role::Access),
        slot_id("alphb", "github", Role::Access)
    );
}

#[test]
fn an_unavailable_store_is_an_error_and_a_damaged_entry_is_another() {
    let store = Arc::new(MemoryStore::new());
    let v = vault(&store, "alpha");
    let id = slot_id("alpha", "github", Role::Access).unwrap();
    store.set(&id, &Secret::new("not json".into())).unwrap();
    assert_eq!(
        v.read(&GITHUB, Role::Access).err(),
        Some(VaultError::Damaged)
    );
    store
        .set(
            &id,
            &Secret::new(r#"{"v":9,"stand_in":"a","real":"b"}"#.into()),
        )
        .unwrap();
    assert_eq!(
        v.read(&GITHUB, Role::Access).err(),
        Some(VaultError::Damaged)
    );
    store.break_it();
    assert_eq!(
        v.read(&GITHUB, Role::Access).err(),
        Some(VaultError::Unavailable)
    );
    assert_eq!(
        v.write(&GITHUB, Role::Access, "s", "r"),
        Err(VaultError::Unavailable)
    );
    assert_eq!(
        v.delete(&GITHUB, Role::Access),
        Err(VaultError::Unavailable)
    );
}
