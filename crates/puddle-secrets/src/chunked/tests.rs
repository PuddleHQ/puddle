// SPDX-License-Identifier: AGPL-3.0-or-later
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;
use crate::store::MemoryStore;

/// A store like Windows': an entry of more than 1280 UTF-16 code units is refused; it can also
/// be made to fail on the nth write.
#[derive(Default)]
struct Limited {
    inner: MemoryStore,
    writes: AtomicUsize,
    fail_at: Mutex<Option<usize>>,
}

impl SecretStore for Limited {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.inner.get(id)
    }

    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        let n = self.writes.fetch_add(1, Ordering::SeqCst);
        if *self.fail_at.lock().unwrap() == Some(n) {
            return Err(StoreError);
        }
        if secret.expose().encode_utf16().count() > 1280 {
            return Err(StoreError);
        }
        self.inner.set(id, secret)
    }

    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.inner.delete(id)
    }
}

fn id(text: &str) -> StoredId {
    StoredId::new(text).unwrap()
}

fn store() -> ChunkedStore<Limited> {
    ChunkedStore::new(Limited::default())
}

fn held(store: &ChunkedStore<Limited>) -> Vec<String> {
    store.inner.inner.ids()
}

fn get(store: &ChunkedStore<Limited>, name: &str) -> Option<String> {
    store.get(&id(name)).unwrap().map(|s| s.expose().to_owned())
}

#[test]
fn a_short_value_is_stored_as_it_is() {
    let store = store();
    store
        .set(&id("env-1"), &Secret::new("short".into()))
        .unwrap();
    assert_eq!(held(&store), ["env-1"]);
    assert_eq!(
        store
            .inner
            .inner
            .get(&id("env-1"))
            .unwrap()
            .unwrap()
            .expose(),
        "short"
    );
    assert_eq!(get(&store, "env-1").as_deref(), Some("short"));
    assert_eq!(get(&store, "env-2"), None);
}

#[test]
fn a_value_the_store_underneath_would_refuse_goes_in_pieces_and_comes_back_whole() {
    let store = store();
    let long: String = (0..8000)
        .map(|i| char::from(b'a' + u8::try_from(i % 26).unwrap()))
        .collect();
    // The store underneath refuses it as one entry.
    assert!(
        store
            .inner
            .set(&id("env-1"), &Secret::new(long.clone()))
            .is_err()
    );
    store.set(&id("env-1"), &Secret::new(long.clone())).unwrap();
    assert_eq!(get(&store, "env-1").as_deref(), Some(long.as_str()));
    // The entry itself holds a head, and eight pieces of at most 1000 units sit beside it.
    let ids = held(&store);
    assert_eq!(ids.len(), 9, "{ids:?}");
    assert!(ids.contains(&"env-1".to_owned()));
    let head = store.inner.inner.get(&id("env-1")).unwrap().unwrap();
    assert!(head.expose().starts_with("puddle-chunks:1:"));
    assert!(!head.expose().contains(&long[..20]));
}

#[test]
fn pieces_are_cut_between_characters_and_counted_in_utf16_units() {
    let text: String = "😀".repeat(1200);
    let pieces = pieces_of(&text);
    assert!(
        pieces
            .iter()
            .all(|p| p.encode_utf16().count() <= CHUNK_UNITS)
    );
    assert_eq!(pieces.concat(), text);
    let store = store();
    store.set(&id("env-1"), &Secret::new(text.clone())).unwrap();
    assert_eq!(get(&store, "env-1").as_deref(), Some(text.as_str()));
    assert_eq!(pieces_of(""), [""]);
    let exact = "x".repeat(CHUNK_UNITS);
    assert_eq!(pieces_of(&exact).len(), 1);
    assert_eq!(pieces_of(&format!("{exact}y")).len(), 2);
}

#[test]
fn a_value_that_looks_like_a_head_is_stored_in_pieces_so_it_is_read_back_as_a_value() {
    let store = store();
    let odd = "puddle-chunks:1:abcd:2";
    store.set(&id("env-1"), &Secret::new(odd.into())).unwrap();
    assert_eq!(get(&store, "env-1").as_deref(), Some(odd));
    assert_eq!(held(&store).len(), 2);
}

#[test]
fn replacing_and_removing_leave_no_pieces_behind() {
    let store = store();
    let long = "x".repeat(3500);
    store.set(&id("env-1"), &Secret::new(long.clone())).unwrap();
    assert_eq!(held(&store).len(), 5);
    // Long to shorter-long: the old generation's pieces go.
    store
        .set(&id("env-1"), &Secret::new("y".repeat(1500)))
        .unwrap();
    assert_eq!(held(&store).len(), 3);
    // Long to short: back to one entry.
    store.set(&id("env-1"), &Secret::new("z".into())).unwrap();
    assert_eq!(held(&store), ["env-1"]);
    assert_eq!(get(&store, "env-1").as_deref(), Some("z"));
    // Short to long, then delete.
    store.set(&id("env-1"), &Secret::new(long)).unwrap();
    store.delete(&id("env-1")).unwrap();
    assert_eq!(held(&store), Vec::<String>::new());
    store.delete(&id("env-1")).unwrap();
    // Another entry is not touched.
    store
        .set(&id("env-2"), &Secret::new("keep".into()))
        .unwrap();
    store
        .set(&id("env-1"), &Secret::new("x".repeat(2500)))
        .unwrap();
    store.delete(&id("env-1")).unwrap();
    assert_eq!(held(&store), ["env-2"]);
}

#[test]
fn a_write_that_fails_half_way_keeps_the_old_value_and_leaves_nothing_behind() {
    let store = store();
    let old = "o".repeat(2500);
    store.set(&id("env-1"), &Secret::new(old.clone())).unwrap();
    let before = held(&store);
    let writes = store.inner.writes.load(Ordering::SeqCst);
    for step in 0..4 {
        *store.inner.fail_at.lock().unwrap() = Some(writes + step);
        let err = store.set(&id("env-1"), &Secret::new("n".repeat(3500)));
        assert_eq!(err, Err(StoreError), "step {step}");
        assert_eq!(
            get(&store, "env-1").as_deref(),
            Some(old.as_str()),
            "step {step}"
        );
        let left = held(&store);
        assert_eq!(left.len(), before.len(), "step {step}: {left:?}");
        store.inner.writes.store(writes, Ordering::SeqCst);
    }
    // The head write (the fifth for a value of four pieces) failing is covered too.
    *store.inner.fail_at.lock().unwrap() = Some(writes + 4);
    assert!(
        store
            .set(&id("env-1"), &Secret::new("n".repeat(3500)))
            .is_err()
    );
    assert_eq!(get(&store, "env-1").as_deref(), Some(old.as_str()));
    assert_eq!(held(&store).len(), before.len());
}

#[test]
fn a_damaged_entry_is_an_error_never_a_shorter_value() {
    let store = store();
    store
        .set(&id("env-1"), &Secret::new("x".repeat(2500)))
        .unwrap();
    let victim = held(&store).into_iter().find(|i| i != "env-1").unwrap();
    store.inner.inner.delete(&id(&victim)).unwrap();
    assert_eq!(store.get(&id("env-1")).unwrap_err(), StoreError);
    // A head that names too many pieces, or has no generation, is just a value.
    for odd in [
        "puddle-chunks:1:zz:2",
        "puddle-chunks:1:ab:0",
        "puddle-chunks:1:ab:65",
        "puddle-chunks:1::2",
    ] {
        store
            .inner
            .inner
            .set(&id("env-2"), &Secret::new(odd.into()))
            .unwrap();
        assert_eq!(get(&store, "env-2").as_deref(), Some(odd));
    }
}

#[test]
fn a_value_needing_more_than_the_most_pieces_is_refused() {
    let store = store();
    let huge = "x".repeat(CHUNK_UNITS * MAX_PIECES + 1);
    assert!(store.set(&id("env-1"), &Secret::new(huge)).is_err());
    assert_eq!(held(&store), Vec::<String>::new());
    let most = "x".repeat(CHUNK_UNITS * MAX_PIECES);
    store.set(&id("env-1"), &Secret::new(most.clone())).unwrap();
    assert_eq!(get(&store, "env-1").as_deref(), Some(most.as_str()));
}

#[test]
fn an_unavailable_store_underneath_is_an_error_at_every_call() {
    let store = ChunkedStore::new(MemoryStore::new());
    store.set(&id("a"), &Secret::new("v".into())).unwrap();
    store.inner.break_it();
    assert!(store.get(&id("a")).is_err());
    assert!(store.set(&id("a"), &Secret::new("v".into())).is_err());
    assert!(store.delete(&id("a")).is_err());
}

#[test]
fn keeping_an_entry_local_reaches_its_head_and_every_piece() {
    let store = ChunkedStore::new(MemoryStore::new());
    let long = "x".repeat(CHUNK_UNITS * 2 + 1);
    let long_id = StoredId::new("long").unwrap();
    store.set(&long_id, &Secret::new(long)).unwrap();
    store.keep_local(&long_id).unwrap();
    let kept = store.inner.kept_local();
    assert_eq!(kept.len(), 4);
    assert_eq!(kept[0], "long");
    assert!(kept[1..].iter().all(|piece| piece.starts_with("long-")));
}

#[test]
fn keeping_all_local_asks_the_store_underneath() {
    assert_eq!(
        ChunkedStore::new(MemoryStore::new()).keep_all_local(),
        Ok(0)
    );
}

/// A store that can rewrite whole entries but not the pieces of a long one.
#[derive(Default)]
struct PiecesStuck(MemoryStore);

impl SecretStore for PiecesStuck {
    fn get(&self, id: &StoredId) -> Result<Option<Secret>, StoreError> {
        self.0.get(id)
    }
    fn set(&self, id: &StoredId, secret: &Secret) -> Result<(), StoreError> {
        self.0.set(id, secret)
    }
    fn delete(&self, id: &StoredId) -> Result<(), StoreError> {
        self.0.delete(id)
    }
    fn keep_local(&self, id: &StoredId) -> Result<(), StoreError> {
        if id.as_str().contains('-') {
            return Err(StoreError);
        }
        Ok(())
    }
    fn keep_all_local(&self) -> Result<usize, StoreError> {
        Ok(3)
    }
}

#[test]
fn keeping_all_local_passes_on_the_count_of_failures() {
    assert_eq!(
        ChunkedStore::new(PiecesStuck::default()).keep_all_local(),
        Ok(3)
    );
}

#[test]
fn a_piece_that_cannot_be_rewritten_is_an_error_for_its_entry() {
    let store = ChunkedStore::new(PiecesStuck::default());
    let long_id = StoredId::new("long").unwrap();
    store
        .set(&long_id, &Secret::new("x".repeat(CHUNK_UNITS + 1)))
        .unwrap();
    assert_eq!(store.keep_local(&long_id), Err(StoreError));
    store.delete(&long_id).unwrap();
}

#[test]
fn keeping_a_short_or_missing_entry_local_touches_only_that_id() {
    let store = ChunkedStore::new(MemoryStore::new());
    let short = StoredId::new("short").unwrap();
    store.set(&short, &Secret::new("v".to_owned())).unwrap();
    store.keep_local(&short).unwrap();
    store
        .keep_local(&StoredId::new("missing").unwrap())
        .unwrap();
    assert_eq!(store.inner.kept_local(), ["short", "missing"]);
}
