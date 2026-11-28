// SPDX-License-Identifier: AGPL-3.0-or-later
//! The backup the API serves: the [`BackupService`] trait the route calls, [`NoBackup`] for a build
//! without it, and [`FakeBackup`] for tests and the UI fixture.
//!
//! What a backup holds is the host's (`puddle-host`, `backup_archive`); this crate only streams the
//! file it makes and names it.

use std::pin::Pin;
#[cfg(any(test, feature = "testing"))]
use std::sync::{Mutex, PoisonError};

use futures_util::future::BoxFuture;
use tokio::io::AsyncRead;

/// Why a backup was not made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BackupServiceError {
    /// Not wired in this build or state (503).
    #[error("{0}")]
    Unavailable(String),
    /// The data could not be read or the file could not be written (503, with the reason).
    #[error("{0}")]
    Failed(String),
}

/// A finished backup, ready to be sent: its size and a reader at its first byte.
pub struct BackupArchive {
    size: u64,
    reader: Pin<Box<dyn AsyncRead + Send>>,
}

impl std::fmt::Debug for BackupArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupArchive")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl BackupArchive {
    /// An archive of `size` bytes that `reader` yields from the start.
    #[must_use]
    pub fn new(size: u64, reader: impl AsyncRead + Send + 'static) -> Self {
        Self {
            size,
            reader: Box::pin(reader),
        }
    }

    /// The archive's size in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.size
    }

    /// The archive's bytes, from the start.
    #[must_use]
    pub fn into_reader(self) -> Pin<Box<dyn AsyncRead + Send>> {
        self.reader
    }
}

/// Makes a backup of Puddle's own state.
pub trait BackupService: Send + Sync {
    /// Makes a backup now: one point in time of the rules, settings, workspace list and the rest of
    /// what the backup page lists, packed in one file. Safe while Puddle runs.
    fn create(&self) -> BoxFuture<'_, Result<BackupArchive, BackupServiceError>>;
}

/// The service when none is wired in: every call says so.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoBackup;

impl BackupService for NoBackup {
    fn create(&self) -> BoxFuture<'_, Result<BackupArchive, BackupServiceError>> {
        Box::pin(async {
            Err(BackupServiceError::Unavailable(
                "backups are not available in this build".into(),
            ))
        })
    }
}

/// The name a download of a backup made at `now_ms` (milliseconds since the Unix epoch) gets:
/// `puddle-backup-20261115T225500Z.tar.gz`. UTC, so a sort by name is a sort by time.
#[must_use]
pub fn archive_file_name(now_ms: u64) -> String {
    let seconds = now_ms / 1000;
    let (days, rest) = (seconds / 86_400, seconds % 86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "puddle-backup-{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z.tar.gz",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The proleptic Gregorian date of `days` since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let day_of_era = z % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + u64::from(month <= 2);
    (year, month, day)
}

#[cfg(any(test, feature = "testing"))]
/// The bytes [`FakeBackup`] serves: not an archive, only a recognisable payload.
pub const FAKE_BACKUP_BYTES: &[u8] = b"Puddle fake backup: not a real archive\n";

#[cfg(any(test, feature = "testing"))]
/// An in-memory [`BackupService`] for tests and the UI fixture: it serves [`FAKE_BACKUP_BYTES`], or
/// the failure set with [`FakeBackup::fail_with`], and counts how many backups were asked for.
#[derive(Debug, Default)]
pub struct FakeBackup {
    state: Mutex<FakeState>,
}

#[cfg(any(test, feature = "testing"))]
#[derive(Debug, Default)]
struct FakeState {
    failure: Option<String>,
    requested: usize,
}

#[cfg(any(test, feature = "testing"))]
impl FakeBackup {
    /// A fake that makes every backup.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes every backup from now on fail with `reason` (`None` makes them succeed again).
    pub fn fail_with(&self, reason: Option<&str>) {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .failure = reason.map(str::to_owned);
    }

    /// How many backups were asked for, failed ones included.
    #[must_use]
    pub fn requested(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .requested
    }
}

#[cfg(any(test, feature = "testing"))]
impl BackupService for FakeBackup {
    fn create(&self) -> BoxFuture<'_, Result<BackupArchive, BackupServiceError>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.requested += 1;
        let result = match &state.failure {
            Some(reason) => Err(BackupServiceError::Failed(reason.clone())),
            None => Ok(BackupArchive::new(
                FAKE_BACKUP_BYTES.len() as u64,
                std::io::Cursor::new(FAKE_BACKUP_BYTES),
            )),
        };
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests;
