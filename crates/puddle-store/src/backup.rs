// SPDX-License-Identifier: GPL-3.0-or-later
//! A consistent copy of the database while the host process keeps writing to it.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

use crate::error::StoreError;

/// How long the copy waits for a writer that holds the database busy (the host's own wait).
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What a finished copy holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupInfo {
    /// The copy's schema version (`PRAGMA user_version`), which the Puddle that made the source
    /// wrote; a restore needs a Puddle that knows it.
    pub schema_version: u32,
}

/// Copies the database at `source` to the new file `dest` as one point in time, and checks the
/// copy.
///
/// The copy leaves out the sign-in secrets (every user's password hash and pending invite): their
/// tables are emptied in the copy, so a restore keeps the users and they get a new invite.
///
/// The copy is `VACUUM INTO` on a read-only connection: one read transaction, so it sees every
/// commit up to its start and nothing half-written, while the host keeps writing (WAL readers
/// never block the writer). A plain file copy of `puddle.db` would miss what still sits in the
/// `-wal` file. The copy is a single file with no `-wal` or `-shm`. Nothing here opens the
/// source for writing or migrates it, so a database from a newer Puddle copies as it is.
///
/// `dest` must not exist; its permissions are whatever the process creates files with, so the
/// caller puts it in an owner-only folder.
///
/// # Errors
///
/// [`StoreError::Database`] when `source` is missing or not a database, `dest` exists or cannot
/// be written, the wait for a writer runs out, or the copy fails its integrity check.
pub fn backup_database(source: &Path, dest: &Path) -> Result<BackupInfo, StoreError> {
    let conn = Connection::open_with_flags(source, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(BUSY_TIMEOUT)?;
    // The target is a bound value, so a path with a quote in it needs no escaping.
    conn.execute("VACUUM INTO ?1", [dest.to_string_lossy().as_ref()])?;
    drop(conn);

    drop_sign_in_secrets(dest)?;
    let copy = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = copy.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    verdict(&check)?;
    let schema_version: u32 = copy.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(BackupInfo { schema_version })
}

/// The tables that hold sign-in secrets (see [`crate::AccountSecrets`]); a backup never keeps
/// their rows.
const SECRET_TABLES: [&str; 2] = ["account_passwords", "account_invites"];

const TABLE_EXISTS: &str =
    "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)";

/// Empties the secret tables of the copy at `dest` and rebuilds the file, so the removed rows do
/// not survive in its free pages. A database from before those tables existed has none to empty.
fn drop_sign_in_secrets(dest: &Path) -> Result<(), StoreError> {
    let copy = Connection::open_with_flags(dest, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    copy.pragma_update(None, "secure_delete", true)?;
    for table in SECRET_TABLES {
        let exists: bool = copy.query_row(TABLE_EXISTS, [table], |row| row.get(0))?;
        if exists {
            copy.execute(&format!("DELETE FROM {table}"), [])?;
        }
    }
    copy.execute("VACUUM", [])?;
    Ok(())
}

/// `Ok` for SQLite's `ok` integrity answer, otherwise the answer as a corruption error.
fn verdict(check: &str) -> Result<(), StoreError> {
    if check == "ok" {
        return Ok(());
    }
    Err(StoreError::Database(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CORRUPT),
        Some(format!("the copy failed its integrity check: {check}")),
    )))
}

#[cfg(test)]
mod tests;
