// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn memory_store_round_trip() {
    let store = MemoryStore::new();
    let id = StoredId::new("a").unwrap();
    assert!(store.get(&id).unwrap().is_none());
    store.set(&id, &Secret::new("v1".into())).unwrap();
    store.set(&id, &Secret::new("v2".into())).unwrap();
    assert_eq!(store.get(&id).unwrap().unwrap().expose(), "v2");
    store.delete(&id).unwrap();
    store.delete(&id).unwrap();
    assert!(store.get(&id).unwrap().is_none());
    store.break_it();
    assert_eq!(store.get(&id).err(), Some(StoreError));
    assert_eq!(store.set(&id, &Secret::new("x".into())), Err(StoreError));
    assert_eq!(store.delete(&id), Err(StoreError));
}
