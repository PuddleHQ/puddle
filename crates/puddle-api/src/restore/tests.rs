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
