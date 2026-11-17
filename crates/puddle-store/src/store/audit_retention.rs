// SPDX-License-Identifier: AGPL-3.0-or-later
//! How long the audit is kept, what it holds, and giving the space of deleted records back.
//!
//! Records go by age only: the retention is a number of days. There is no cap per workspace and no
//! size cap that deletes, because a workspace that floods its own log could then push out the
//! records it wants gone. The size only warns ([`super::Limits::audit_warn_bytes`]).

use std::sync::atomic::Ordering;

use rusqlite::{Connection, OptionalExtension, params};

use crate::audit::AuditRecord;
use crate::error::StoreError;

use super::audit::{append, audit_head};
use super::{Store, lock, sql_ts, write_tx};

/// How many days the audit is kept until the owner changes it.
pub const DEFAULT_AUDIT_RETENTION_DAYS: u32 = 90;

/// The longest retention that can be set: ten years.
pub const MAX_AUDIT_RETENTION_DAYS: u32 = 3650;

/// The sweeper looks for records past the retention at most this often (one hour).
const PRUNE_EVERY_MS: u64 = 60 * 60 * 1000;

/// A prune deletes this many records per transaction, so a writer is never held for long.
const CHUNK: i64 = 5000;

/// The database file is shrunk once at least this many bytes of it are free (64 MiB) ...
const SHRINK_MIN_FREE_BYTES: u64 = 67_108_864;

/// ... or a quarter of the file is free.
const SHRINK_MIN_FREE_PERCENT: u64 = 25;

/// Pages freed per `incremental_vacuum` step, and steps per sweep, so one sweep gives back at
/// most 2048 x 400 pages and never holds the writer for long at a time.
const SHRINK_STEP_PAGES: u32 = 2048;
const SHRINK_STEPS_PER_SWEEP: u32 = 400;

/// SQLite's `auto_vacuum` value for incremental mode.
const AUTO_VACUUM_INCREMENTAL: i64 = 2;

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// [`Store::last_audit_prune`] before any prune.
pub(super) const NEVER: u64 = u64::MAX;

/// What a prune deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AuditPrune {
    /// Records deleted.
    pub deleted: u64,
}

/// What the audit holds and how fast it grows, for the admin to judge its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditStats {
    /// Records held.
    pub records: u64,
    /// Bytes of their lines (not the file: indexes and free space come on top).
    pub bytes: u64,
    /// `ts` of the oldest record, epoch ms; `None` when there are none.
    pub oldest_ts: Option<u64>,
    /// Records written in the last 24 hours.
    pub records_last_day: u64,
    /// Bytes of the lines written in the last 24 hours.
    pub bytes_last_day: u64,
    /// Records written in the last 7 days.
    pub records_last_week: u64,
    /// Bytes of the lines written in the last 7 days.
    pub bytes_last_week: u64,
    /// The days the audit is kept.
    pub retention_days: u32,
    /// The size of the lines above which Puddle warns.
    pub warn_bytes: u64,
    /// The size of the database file (everything in it, not only the audit).
    pub file_bytes: u64,
    /// The part of the file that is free space, which a shrink gives back.
    pub free_bytes: u64,
}

fn pragma_u64(conn: &Connection, name: &str) -> Result<u64, StoreError> {
    let value: i64 = conn.pragma_query_value(None, name, |row| row.get(0))?;
    Ok(u64::try_from(value).unwrap_or(0))
}

/// How many records, and how many bytes of their lines, have a `ts` of `since` or later.
fn written_since(conn: &Connection, since: i64) -> Result<(u64, u64), StoreError> {
    let (count, bytes): (i64, i64) =
        conn.query_row(WRITTEN_SINCE, [since], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok((
        u64::try_from(count).unwrap_or(0),
        u64::try_from(bytes).unwrap_or(0),
    ))
}

const WRITTEN_SINCE: &str = "SELECT count(*), coalesce(sum(length(CAST(line AS BLOB))), 0)
                             FROM audit WHERE ts >= ?1";

impl Store {
    /// The days the audit is kept ([`DEFAULT_AUDIT_RETENTION_DAYS`] until saved).
    ///
    /// # Errors
    /// A database error.
    pub fn audit_retention_days(&self) -> Result<u32, StoreError> {
        let days: Option<i64> = lock(&self.conn)
            .query_row("SELECT days FROM audit_retention WHERE id = 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        Ok(days
            .and_then(|days| u32::try_from(days).ok())
            .unwrap_or(DEFAULT_AUDIT_RETENTION_DAYS))
    }

    /// Saves how many days the audit is kept. The sweeper deletes what is older at its next pass
    /// that looks (within the hour); nothing is deleted here.
    ///
    /// # Errors
    /// [`StoreError::AuditRetention`] for `days` outside 1 to [`MAX_AUDIT_RETENTION_DAYS`], or a
    /// database error.
    pub fn set_audit_retention_days(&self, days: u32) -> Result<(), StoreError> {
        if !(1..=MAX_AUDIT_RETENTION_DAYS).contains(&days) {
            return Err(StoreError::AuditRetention(days));
        }
        lock(&self.conn).execute(
            "INSERT INTO audit_retention (id, days) VALUES (1, ?1)
             ON CONFLICT (id) DO UPDATE SET days = ?1",
            [days],
        )?;
        // Look again at the next sweep instead of waiting out the hour.
        self.last_audit_prune.store(NEVER, Ordering::Relaxed);
        Ok(())
    }

    /// What the audit holds and how fast it grows.
    ///
    /// # Errors
    /// A database error.
    pub fn audit_stats(&self) -> Result<AuditStats, StoreError> {
        let retention_days = self.audit_retention_days()?;
        let now = self.clock.now_ms();
        let conn = lock(&self.conn);
        let records: i64 = conn.query_row("SELECT count(*) FROM audit", [], |row| row.get(0))?;
        let oldest: Option<i64> =
            conn.query_row("SELECT min(ts) FROM audit", [], |row| row.get(0))?;
        let since = |ms: u64| written_since(&conn, sql_ts(now.saturating_sub(ms)));
        let (records_last_day, bytes_last_day) = since(DAY_MS)?;
        let (records_last_week, bytes_last_week) = since(7 * DAY_MS)?;
        let page_size = pragma_u64(&conn, "page_size")?;
        Ok(AuditStats {
            records: u64::try_from(records).unwrap_or(0),
            bytes: super::audit::audit_bytes(&conn)?,
            oldest_ts: oldest.and_then(|ts| u64::try_from(ts).ok()),
            records_last_day,
            bytes_last_day,
            records_last_week,
            bytes_last_week,
            retention_days,
            warn_bytes: self.limits.audit_warn_bytes,
            file_bytes: pragma_u64(&conn, "page_count")?.saturating_mul(page_size),
            free_bytes: pragma_u64(&conn, "freelist_count")?.saturating_mul(page_size),
        })
    }

    /// Deletes the audit records older than `keep_days` days, in chunks, and records that it did
    /// when it deleted any. Records of any age that are newer are never touched, whoever's they
    /// are. The file keeps its size until [`Store::shrink_audit_file`].
    ///
    /// # Errors
    /// A database error; the chunks already deleted stay deleted.
    pub fn prune_audit(&self, keep_days: u32) -> Result<AuditPrune, StoreError> {
        let now = self.clock.now_ms();
        let cutoff = sql_ts(now.saturating_sub(u64::from(keep_days).saturating_mul(DAY_MS)));
        let mut deleted = 0u64;
        loop {
            let mut conn = lock(&self.conn);
            let tx = write_tx(&mut conn)?;
            let chunk = tx.execute(
                "DELETE FROM audit WHERE id IN
                     (SELECT id FROM audit WHERE ts < ?1 ORDER BY ts LIMIT ?2)",
                params![cutoff, CHUNK],
            )?;
            tx.commit()?;
            if chunk == 0 {
                break;
            }
            deleted += u64::try_from(chunk).unwrap_or(0);
        }
        if deleted == 0 {
            return Ok(AuditPrune::default());
        }
        let mut conn = lock(&self.conn);
        let tx = write_tx(&mut conn)?;
        let head = audit_head(&tx)?;
        let oldest: Option<i64> =
            tx.query_row("SELECT min(ts) FROM audit", [], |row| row.get(0))?;
        append(
            &tx,
            &AuditRecord::AuditTrimmed {
                ts: now,
                deleted_records: deleted,
                oldest_ts_kept: oldest.and_then(|ts| u64::try_from(ts).ok()),
            },
        )?;
        self.commit(tx, head, Vec::new())?;
        tracing::info!(
            deleted,
            keep_days,
            "audit records past their retention deleted"
        );
        Ok(AuditPrune { deleted })
    }

    /// The sweeper's prune: by the saved retention, at most once an hour (and at once at start and
    /// after the retention changes).
    pub(super) fn prune_audit_if_due(&self) -> Result<u64, StoreError> {
        let now = self.clock.now_ms();
        let last = self.last_audit_prune.load(Ordering::Relaxed);
        if last != NEVER && now >= last && now - last < PRUNE_EVERY_MS {
            return Ok(0);
        }
        let pruned = self.prune_audit(self.audit_retention_days()?)?;
        // Only a pass that worked counts, so a failed one is tried again at the next sweep.
        self.last_audit_prune.store(now, Ordering::Relaxed);
        Ok(pruned.deleted)
    }

    /// Gives the free space the deletes left back to the disk, when a good part of the file is
    /// free. Returns the bytes the file shrank by.
    ///
    /// The space is freed in short steps, so writers wait for one step at a time, never for the
    /// whole file. A file that is not in incremental mode yet (made by an older Puddle) is left
    /// alone here: moving it takes a `VACUUM` of the whole file, which [`Store::open`] does
    /// before anything is served (`rebuild_older_file`).
    ///
    /// # Errors
    /// A database error.
    pub fn shrink_audit_file(&self) -> Result<u64, StoreError> {
        self.shrink_file(SHRINK_MIN_FREE_BYTES)
    }

    pub(super) fn shrink_file(&self, min_free_bytes: u64) -> Result<u64, StoreError> {
        let before = {
            let conn = lock(&self.conn);
            let Some(total) = worth_shrinking(&conn, min_free_bytes)? else {
                return Ok(0);
            };
            let mode: i64 = conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
            if mode != AUTO_VACUUM_INCREMENTAL {
                return Ok(0);
            }
            total
        };
        for _ in 0..SHRINK_STEPS_PER_SWEEP {
            let conn = lock(&self.conn);
            conn.execute_batch(&format!("PRAGMA incremental_vacuum({SHRINK_STEP_PAGES})"))?;
            if pragma_u64(&conn, "freelist_count")? == 0 {
                break;
            }
        }
        let conn = lock(&self.conn);
        let page_size = pragma_u64(&conn, "page_size")?;
        let after = pragma_u64(&conn, "page_count")?.saturating_mul(page_size);
        Ok(before.saturating_sub(after))
    }
}

/// The file's size in bytes when enough of it is free to be worth giving back: at least
/// `min_free_bytes`, or [`SHRINK_MIN_FREE_PERCENT`] of the file.
fn worth_shrinking(conn: &Connection, min_free_bytes: u64) -> Result<Option<u64>, StoreError> {
    let page_size = pragma_u64(conn, "page_size")?;
    let total = pragma_u64(conn, "page_count")?.saturating_mul(page_size);
    let free = pragma_u64(conn, "freelist_count")?.saturating_mul(page_size);
    let worth = free > 0
        && (free >= min_free_bytes
            || free.saturating_mul(100) >= total.saturating_mul(SHRINK_MIN_FREE_PERCENT));
    Ok(worth.then_some(total))
}

/// [`rebuild_older_file`], logging the outcome: a rebuild that fails (no room for the copy,
/// another process holding the file) costs only the space, so the open goes on.
pub(super) fn rebuild_older_file_on_open(conn: &Connection) {
    match rebuild_older_file(conn) {
        Ok(0) => {}
        Ok(bytes) => tracing::info!(
            bytes,
            "audit database file rebuilt to give back its free space"
        ),
        Err(error) => tracing::warn!(
            %error,
            "audit database file not rebuilt; the next open tries again"
        ),
    }
}

/// Moves a file an older Puddle made to incremental mode with one `VACUUM` (which needs room for
/// a copy), once it has space worth giving back; files made since are incremental from the
/// start. Runs from [`Store::open`], before anything waits on the connection. Returns the bytes
/// the file shrank by.
fn rebuild_older_file(conn: &Connection) -> Result<u64, StoreError> {
    let mode: i64 = conn.pragma_query_value(None, "auto_vacuum", |row| row.get(0))?;
    if mode == AUTO_VACUUM_INCREMENTAL {
        return Ok(0);
    }
    let Some(total) = worth_shrinking(conn, SHRINK_MIN_FREE_BYTES)? else {
        return Ok(0);
    };
    conn.pragma_update(None, "auto_vacuum", AUTO_VACUUM_INCREMENTAL)?;
    conn.execute("VACUUM", [])?;
    let page_size = pragma_u64(conn, "page_size")?;
    Ok(total.saturating_sub(pragma_u64(conn, "page_count")?.saturating_mul(page_size)))
}

#[cfg(test)]
mod tests;
