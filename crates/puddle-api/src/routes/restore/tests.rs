// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const _: () = assert!(MAX_UPLOAD_BYTES > 4 * 1024 * 1024 * 1024);
const _: () = assert!(MAX_UPLOAD_BYTES < 5 * 1024 * 1024 * 1024);

use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

use puddle_secrets::{MemoryStore, Secret, SecretStore, StoreError, StoredId};
use puddle_services::error::ErrorCode;
use puddle_services::restore::{FAKE_RESTORE_MAGIC, FakeRestore, IdentityTokenRef, SecretRef};

/// A body that arrives as pieces of the given sizes, the first starting like a fake backup.
fn pieces(sizes: &[usize]) -> Body {
    let mut first = true;
    let chunks: Vec<Result<Vec<u8>, io::Error>> = sizes
        .iter()
        .map(|size| {
            let mut piece = if first {
                FAKE_RESTORE_MAGIC.to_vec()
            } else {
                Vec::new()
            };
            first = false;
            piece.resize(*size, b'.');
            Ok(piece)
        })
        .collect();
    Body::from_stream(futures_util::stream::iter(chunks))
}

#[tokio::test]
async fn an_upload_of_exactly_the_limit_is_staged_in_full() {
    let fake = FakeRestore::new();
    let first = FAKE_RESTORE_MAGIC.len() + 3;
    let facts = stage_upload(&fake, pieces(&[first, 7, 5]), first + 12).await;
    assert!(facts.is_ok(), "{:?}", facts.err());
    assert_eq!(fake.last_upload_bytes(), first + 12);
}

#[tokio::test]
async fn an_upload_one_byte_over_the_limit_is_a_413_and_nothing_is_staged() {
    let fake = FakeRestore::new();
    let first = FAKE_RESTORE_MAGIC.len() + 3;
    let error = stage_upload(&fake, pieces(&[first, 7, 6]), first + 12)
        .await
        .expect_err("one byte over");
    assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error.code(), ErrorCode::PayloadTooLarge);
    assert_eq!(fake.previews(), 0, "the service must not have accepted it");
}

#[tokio::test]
async fn a_single_piece_over_the_limit_is_a_413_too() {
    let fake = FakeRestore::new();
    let size = FAKE_RESTORE_MAGIC.len() + 4;
    let error = stage_upload(&fake, pieces(&[size]), size - 1)
        .await
        .expect_err("over");
    assert_eq!(error.status(), StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn an_upload_that_fails_for_another_reason_is_not_a_413() {
    let fake = FakeRestore::new();
    let broken: Vec<Result<Vec<u8>, io::Error>> = vec![Err(io::Error::other("reset"))];
    let error = stage_upload(
        &fake,
        Body::from_stream(futures_util::stream::iter(broken)),
        100,
    )
    .await
    .expect_err("reset");
    assert_eq!(error.status(), StatusCode::SERVICE_UNAVAILABLE);
}

fn secret_ref(id: &str) -> SecretRef {
    SecretRef {
        workspace_id: Some("w-one".into()),
        name: format!("NAME_{id}"),
        secret_id: id.into(),
    }
}

fn token_ref(id: &str) -> IdentityTokenRef {
    IdentityTokenRef {
        identity: format!("who-{id}"),
        host: "github.com".into(),
        secret_id: id.into(),
    }
}

#[tokio::test]
async fn only_the_secrets_and_tokens_without_a_value_here_are_listed() {
    let store = Arc::new(MemoryStore::new());
    let have = StoredId::new("env-have").unwrap();
    store.set(&have, &Secret::new("v".into())).unwrap();
    let have_token = StoredId::new("tok-have").unwrap();
    store.set(&have_token, &Secret::new("v".into())).unwrap();
    let (secrets, tokens) = missing_values(
        store,
        vec![
            secret_ref("env-have"),
            secret_ref("env-gone"),
            secret_ref("not a stored id!"),
        ],
        vec![
            token_ref("tok-have"),
            token_ref("tok-gone"),
            token_ref("bad id!"),
        ],
    )
    .await
    .unwrap();
    let names: Vec<_> = secrets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["NAME_env-gone", "NAME_not a stored id!"]);
    assert_eq!(secrets[0].workspace_id.as_deref(), Some("w-one"));
    let who: Vec<_> = tokens.iter().map(|t| t.identity.as_str()).collect();
    assert_eq!(who, ["who-tok-gone", "who-bad id!"]);
    assert_eq!(tokens[0].host, "github.com");
}

/// A store whose read waits for a signal that only the async worker can send: a read made on the
/// worker itself never gets it.
struct GatedStore(std::sync::Mutex<Receiver<()>>);

impl SecretStore for GatedStore {
    fn get(&self, _: &StoredId) -> Result<Option<Secret>, StoreError> {
        let gate = self.0.lock().unwrap();
        Ok(gate
            .recv_timeout(Duration::from_secs(5))
            .ok()
            .map(|()| Secret::new("v".into())))
    }
    fn set(&self, _: &StoredId, _: &Secret) -> Result<(), StoreError> {
        Ok(())
    }
    fn delete(&self, _: &StoredId) -> Result<(), StoreError> {
        Ok(())
    }
}

#[tokio::test]
async fn the_credential_store_is_read_off_the_async_worker() {
    let (open, gate) = channel();
    let store = Arc::new(GatedStore(std::sync::Mutex::new(gate)));
    let opener = tokio::spawn(async move { open.send(()).unwrap() });
    let (secrets, _) = missing_values(store, vec![secret_ref("env-a")], Vec::new())
        .await
        .unwrap();
    opener.await.unwrap();
    assert!(secrets.is_empty(), "the read waited on the async worker");
}
