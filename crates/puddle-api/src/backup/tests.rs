// SPDX-License-Identifier: AGPL-3.0-or-later
use tokio::io::AsyncReadExt as _;

use super::*;

#[tokio::test]
async fn without_a_service_a_backup_says_it_is_unavailable() {
    let err = NoBackup.create().await.unwrap_err();
    assert!(matches!(err, BackupServiceError::Unavailable(_)), "{err}");
    assert!(err.to_string().contains("not available"), "{err}");
}

#[tokio::test]
async fn the_fake_serves_its_bytes_counts_requests_and_can_fail() {
    let fake = FakeBackup::new();
    let archive = fake.create().await.unwrap();
    assert_eq!(archive.size(), FAKE_BACKUP_BYTES.len() as u64);
    assert!(format!("{archive:?}").starts_with("BackupArchive"));
    let mut body = Vec::new();
    archive.into_reader().read_to_end(&mut body).await.unwrap();
    assert_eq!(body, FAKE_BACKUP_BYTES);

    fake.fail_with(Some("the disk is full"));
    let err = fake.create().await.unwrap_err();
    assert_eq!(err, BackupServiceError::Failed("the disk is full".into()));
    fake.fail_with(None);
    assert!(fake.create().await.is_ok());
    assert_eq!(fake.requested(), 3);
}

#[test]
fn the_file_name_is_the_utc_time_and_sorts_by_time() {
    assert_eq!(
        archive_file_name(0),
        "puddle-backup-19700101T000000Z.tar.gz"
    );
    // 2026-11-15T22:55:00Z, and a leap day: 2024-02-29T23:59:59Z.
    assert_eq!(
        archive_file_name(1_794_783_300_000),
        "puddle-backup-20261115T225500Z.tar.gz"
    );
    assert_eq!(
        archive_file_name(1_709_251_199_999),
        "puddle-backup-20240229T235959Z.tar.gz"
    );
    assert!(archive_file_name(1_000) < archive_file_name(1_000_000_000_000));
}
