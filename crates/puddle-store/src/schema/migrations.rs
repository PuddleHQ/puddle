// SPDX-License-Identifier: AGPL-3.0-or-later
//! Runs the steps in [`super::MIGRATIONS`]: the version check, the write wait, the copy kept
//! before an upgrade and its pruning.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, TransactionBehavior};

use super::MIGRATIONS;
use crate::error::StoreError;

/// Brings `conn` to [`SCHEMA_VERSION`] and returns the version it started at.
///
/// # Errors
/// [`StoreError::SchemaTooNew`] for a database written by a newer Puddle (never downgraded),
/// or the SQLite error of a failed migration (which is rolled back).
pub(crate) fn migrate(conn: &mut Connection) -> Result<u32, StoreError> {
    migrate_with(conn, MIGRATIONS)
}

/// Migrates only up to `version`, to build the database an older Puddle left behind.
#[cfg(test)]
pub(crate) fn migrate_up_to(conn: &mut Connection, version: usize) -> Result<u32, StoreError> {
    migrate_with(conn, &MIGRATIONS[..version])
}

/// How many `puddle.db.pre-v<N>` copies stay next to the database.
pub(super) const BACKUPS_KEPT: usize = 2;

/// One row of `PRAGMA foreign_key_check`: (child table, child rowid, parent table, constraint).
pub(super) type Violation = (String, Option<i64>, String, i64);

/// How long an upgrade waits for another process that holds the file (the CLI or a host that is
/// upgrading it too). The normal write wait is 5 s; an upgrade can run far longer, and the
/// process that lost the race has nothing to serve until the file is current.
pub(super) const UPGRADE_WAIT: std::time::Duration = std::time::Duration::from_secs(300);

pub(super) fn migrate_with(conn: &mut Connection, migrations: &[&str]) -> Result<u32, StoreError> {
    let normal: i64 = conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
    conn.busy_timeout(UPGRADE_WAIT)?;
    let result = migrate_loop(conn, migrations);
    let restored = conn.busy_timeout(std::time::Duration::from_millis(
        u64::try_from(normal).unwrap_or(0),
    ));
    result.and_then(|start| restored.map(|()| start).map_err(StoreError::from))
}

pub(super) fn migrate_loop(conn: &mut Connection, migrations: &[&str]) -> Result<u32, StoreError> {
    let supported = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
    loop {
        let start = current_version(conn, supported)?;
        if start == supported {
            return Ok(start);
        }
        // The copy is taken before the write lock (`VACUUM INTO` cannot run inside a
        // transaction) and only put in place under it, once the version is seen unchanged: a
        // process that lost the race to another one must not replace that one's copy with the
        // file it already upgraded.
        let copy = if start > 0 {
            copy_before(conn, supported)?
        } else {
            None
        };
        // The pragma is a no-op inside a transaction, so it is switched here. With it on, a table
        // rebuild (`DROP TABLE` of a parent) cascades deletes into the child tables.
        let foreign_keys: bool = conn.pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
        conn.pragma_update(None, "foreign_keys", false)?;
        let result = run_steps(conn, migrations, start, copy.as_ref());
        if foreign_keys {
            conn.pragma_update(None, "foreign_keys", true)?;
        }
        if let Some(copy) = &copy {
            copy.discard();
        }
        if result? {
            return Ok(start);
        }
        // Another process moved the file on since `start` was read: look again.
    }
}

/// The file's version, refused when a newer Puddle wrote it.
pub(super) fn current_version(conn: &Connection, supported: u32) -> Result<u32, StoreError> {
    let found: u32 = conn.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if found > supported {
        return Err(StoreError::SchemaTooNew { found, supported });
    }
    Ok(found)
}

/// Runs the steps after `start` in one write transaction. Returns `false`, changing nothing,
/// when the file is no longer at `start` once the lock is held.
pub(super) fn run_steps(
    conn: &mut Connection,
    migrations: &[&str],
    start: u32,
    copy: Option<&PreUpgradeCopy>,
) -> Result<bool, StoreError> {
    let supported = u32::try_from(migrations.len()).unwrap_or(u32::MAX);
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if current_version(&tx, supported)? != start {
        return Ok(false);
    }
    // Violations a step did not cause (an older bug, a hand edit) never block the upgrade.
    let known = violations(&tx)?;
    if let Some(copy) = copy {
        copy.put_in_place()?;
    }
    for (version, sql) in (start..).zip(migrations.iter().skip(start as usize)) {
        tx.execute_batch(sql)?;
        if let Some((table, _, parent, _)) = violations(&tx)?
            .into_iter()
            .find(|found| !known.contains(found))
        {
            return Err(StoreError::MigrationBrokeForeignKeys {
                version: version + 1,
                table,
                parent,
            });
        }
    }
    tx.pragma_update(None, "user_version", supported)?;
    tx.commit()?;
    Ok(true)
}

pub(super) fn violations(conn: &Connection) -> Result<Vec<Violation>, StoreError> {
    let mut stmt = conn.prepare("PRAGMA foreign_key_check")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// A copy of the database as it was before the steps, made under a name of its own and put in
/// place as `<file>.pre-v<target>` only under the write lock that runs the steps, so a bad
/// release can be rolled back by putting the copy in place.
pub(super) struct PreUpgradeCopy {
    pub(super) made: PathBuf,
    pub(super) db: PathBuf,
    pub(super) target: u32,
}

impl PreUpgradeCopy {
    /// Renames the copy to `<file>.pre-v<target>` (replacing a copy a failed earlier run left)
    /// and deletes all but the newest [`BACKUPS_KEPT`] copies.
    fn put_in_place(&self) -> Result<(), StoreError> {
        let backup = backup_path(&self.db, self.target);
        std::fs::rename(&self.made, &backup)
            .map_err(|err| StoreError::Backup(format!("{}: {err}", backup.display())))?;
        prune_backups(&self.db).map_err(|err| StoreError::Backup(err.to_string()))
    }

    /// Deletes the copy if it was not put in place.
    pub(super) fn discard(&self) {
        match std::fs::remove_file(&self.made) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(%error, path = %self.made.display(), "copy not deleted"),
        }
    }
}

/// Copies the database under a name no other process uses. In-memory databases have nothing to
/// copy.
pub(super) fn copy_before(
    conn: &Connection,
    target: u32,
) -> Result<Option<PreUpgradeCopy>, StoreError> {
    static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let Some(path) = conn.path().filter(|p| !p.is_empty() && *p != ":memory:") else {
        return Ok(None);
    };
    let db = PathBuf::from(path);
    let mut name = backup_path(&db, target).into_os_string();
    name.push(format!(
        ".{}-{}.partial",
        std::process::id(),
        MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let copy = PreUpgradeCopy {
        made: PathBuf::from(name),
        db,
        target,
    };
    // `VACUUM INTO` refuses an existing file.
    copy.discard();
    conn.execute("VACUUM INTO ?1", [copy.made.to_string_lossy().as_ref()])
        .inspect_err(|_| copy.discard())?;
    Ok(Some(copy))
}

pub(super) fn backup_path(db: &Path, version: u32) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(format!(".pre-v{version}"));
    PathBuf::from(name)
}

/// A half-made copy nobody has written to for this long belongs to a process that died (a live
/// `VACUUM INTO` keeps growing its file).
pub(super) const STALE_PARTIAL: std::time::Duration = std::time::Duration::from_secs(3600);

/// A copy idle for [`STALE_PARTIAL`] or longer is a crash leftover.
pub(super) fn partial_copy_is_stale(idle: std::time::Duration) -> bool {
    idle >= STALE_PARTIAL
}

/// Deletes the `<file>.pre-v<N>.<pid>-<n>.partial` files a crash during the pre-upgrade copy
/// left next to `db`. A file written to within [`STALE_PARTIAL`] may be another process's copy in
/// progress and stays. Failures are logged; the open goes on.
pub(crate) fn remove_stale_partial_copies(db: &Path) {
    let dir = db
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let prefix = format!(
        "{}.pre-v",
        db.file_name().unwrap_or_default().to_string_lossy()
    );
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(&prefix) && name.ends_with(".partial")) {
            continue;
        }
        let idle = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|written| written.elapsed().ok());
        if !idle.is_some_and(partial_copy_is_stale) {
            continue;
        }
        if let Err(error) = std::fs::remove_file(entry.path()) {
            tracing::warn!(%error, path = %entry.path().display(), "half-made upgrade copy not deleted");
        }
    }
}

/// Deletes all but the newest [`BACKUPS_KEPT`] copies next to `db`.
pub(super) fn prune_backups(db: &Path) -> std::io::Result<()> {
    let dir = db
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file = db.file_name().unwrap_or_default();
    let prefix = format!("{}.pre-v", file.to_string_lossy());
    let mut copies: Vec<(u32, PathBuf)> = std::fs::read_dir(dir)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let version = name.strip_prefix(&prefix)?.parse().ok()?;
            Some((version, entry.path()))
        })
        .collect();
    copies.sort_unstable_by_key(|copy| std::cmp::Reverse(copy.0));
    for (_, stale) in copies.into_iter().skip(BACKUPS_KEPT) {
        // Name the file that failed, not the copy just made.
        std::fs::remove_file(&stale).map_err(|err| {
            std::io::Error::new(err.kind(), format!("{}: {err}", stale.display()))
        })?;
    }
    Ok(())
}
