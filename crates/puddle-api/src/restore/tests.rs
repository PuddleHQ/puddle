// SPDX-License-Identifier: AGPL-3.0-or-later
use futures_util::StreamExt as _;

use super::*;

fn upload(bytes: &[u8]) -> UploadStream {
    futures_util::stream::iter([Ok(bytes.to_vec())]).boxed()
}

#[tokio::test]
async fn no_service_says_restore_is_unavailable() {
    let err = NoRestore.preview(upload(b"x")).await.unwrap_err();
    assert!(matches!(err, RestoreServiceError::Unavailable(_)), "{err}");
}

#[tokio::test]
async fn the_fake_accepts_its_magic_and_refuses_anything_else() {
    let fake = FakeRestore::new();
    let facts = fake.preview(upload(FAKE_RESTORE_MAGIC)).await.unwrap();
    assert_eq!(facts.parts[0].name, RestorePartName::Rules);
    let err = fake.preview(upload(b"nope")).await.unwrap_err();
    assert!(matches!(err, RestoreServiceError::Refused(_)), "{err}");
    assert_eq!(fake.previews(), 2);
}

#[tokio::test]
async fn a_failing_upload_is_a_failure_not_a_refusal() {
    let broken =
        futures_util::stream::iter([Err(std::io::Error::other("connection reset"))]).boxed();
    let err = FakeRestore::new().preview(broken).await.unwrap_err();
    assert!(matches!(err, RestoreServiceError::Failed(m) if m.contains("connection reset")));
}

#[tokio::test]
async fn a_service_with_nothing_wired_says_so_for_every_call() {
    let staging = "0".repeat(32);
    let unavailable = |e: RestoreServiceError| matches!(e, RestoreServiceError::Unavailable(_));
    assert!(unavailable(
        NoRestore
            .apply(&staging, RestoreMode::Add)
            .await
            .unwrap_err()
    ));
    assert!(unavailable(NoRestore.result().await.unwrap_err()));
    assert!(unavailable(NoRestore.dismiss_result().await.unwrap_err()));
    assert!(unavailable(NoRestore.safety_copies().await.unwrap_err()));
    assert!(unavailable(
        NoRestore.delete_safety_copy("x").await.unwrap_err()
    ));
}

#[tokio::test]
async fn the_fake_default_is_the_fake_and_forgets_a_result_when_dismissed() {
    let fake = FakeRestore::default();
    assert_eq!(fake.previews(), 0);
    fake.set_result(Some(RestoreResult {
        applied: true,
        mode: RestoreMode::Add,
        safety_copy: "x".into(),
        at_ms: 1,
        message: "m".into(),
    }));
    assert!(fake.result().await.unwrap().is_some());
    fake.dismiss_result().await.unwrap();
    assert!(fake.result().await.unwrap().is_none());
}
