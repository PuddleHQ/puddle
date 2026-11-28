// SPDX-License-Identifier: AGPL-3.0-or-later
//! How long the audit is kept, what it holds, and giving the space of deleted records back.
//!
//! Records go by age only: the retention is a number of days. There is no cap per workspace and no
//! size cap that deletes, because a workspace that floods its own log could then push out the
//! records it wants gone. The size only warns ([`super::Limits::audit_warn_bytes`]).

use std::sync::atomic::Ordering;
use std::time::Duration;

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

/// The most the write-ahead log file keeps on disk once a checkpoint has emptied it (64 MiB).
/// Without it SQLite reuses the log in place and the file never shrinks below its peak.
pub(super) const WAL_LIMIT_BYTES: i64 = 67_108_864;

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

/// Frees up to `pages` pages from the end of the file. `incremental_vacuum` frees one page per
/// step of its statement, so the statement is stepped until it is done: a single step, as
/// `Connection::execute_batch` takes, frees one page.
fn free_pages(conn: &Connection, pages: u32) -> Result<(), StoreError> {
    let mut statement = conn.prepare(&format!("PRAGMA incremental_vacuum({pages})"))?;
    let mut rows = statement.query([])?;
    while rows.next()?.is_some() {}
    Ok(())
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

fn retention_days(conn: &Connection) -> Result<u32, StoreError> {
    let days: Option<i64> = conn
        .query_row("SELECT days FROM audit_retention WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(days
        .and_then(|days| u32::try_from(days).ok())
        .unwrap_or(DEFAULT_AUDIT_RETENTION_DAYS))
}

impl Store {
    /// The days the audit is kept ([`DEFAULT_AUDIT_RETENTION_DAYS`] until saved).
    ///
    /// # Errors
    /// A database error.
    pub fn audit_retention_days(&self) -> Result<u32, StoreError> {
        retention_days(&lock(&self.conn))
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
        let now = self.clock.now_ms();
        // The scans run on the audit reader, so they never hold up the writes that wait on the
        // writer's lock (RS-14).
        let conn = self.audit_read();
        let retention_days = retention_days(&conn)?;
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
    /// Each chunk and its `audit_trimmed` record commit in one transaction, so a delete is never
    /// without its record: a record that cannot be written leaves the chunk in place.
    ///
    /// # Errors
    /// A database error; the chunks already deleted stay deleted, each with its record.
    pub fn prune_audit(&self, keep_days: u32) -> Result<AuditPrune, StoreError> {
        let now = self.clock.now_ms();
        let cutoff = sql_ts(now.saturating_sub(u64::from(keep_days).saturating_mul(DAY_MS)));
        let mut deleted = 0u64;
        loop {
            let mut conn = lock(&self.conn);
            let tx = write_tx(&mut conn)?;
            let head = audit_head(&tx)?;
            let chunk = tx.execute(
                "DELETE FROM audit WHERE id IN
                     (SELECT id FROM audit WHERE ts < ?1 ORDER BY ts LIMIT ?2)",
                params![cutoff, CHUNK],
            )?;
            if chunk == 0 {
                break;
            }
            let chunk = u64::try_from(chunk).unwrap_or(0);
            let oldest: Option<i64> =
                tx.query_row("SELECT min(ts) FROM audit", [], |row| row.get(0))?;
            append(
                &tx,
                &AuditRecord::AuditTrimmed {
                    ts: now,
                    deleted_records: chunk,
                    oldest_ts_kept: oldest.and_then(|ts| u64::try_from(ts).ok()),
                },
            )?;
            self.commit(tx, head, Vec::new())?;
            deleted += chunk;
        }
        if deleted > 0 {
            tracing::info!(
                deleted,
                keep_days,
                "audit records past their retention deleted"
            );
        }
        Ok(AuditPrune { deleted })
    }

    /// Whether a prune at `cutoff` would leave the audit empty: it holds records, and all are older.
    fn prune_would_empty_audit(&self, cutoff: i64) -> Result<bool, StoreError> {
        let conn = lock(&self.conn);
        Ok(conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM audit WHERE ts < ?1)
                    AND NOT EXISTS (SELECT 1 FROM audit WHERE ts >= ?1)",
            [cutoff],
            |row| row.get(0),
        )?)
    }

    /// The sweeper's prune: by the saved retention, at most once an hour (and at once at start and
    /// after the retention changes).
    ///
    /// A pass that would delete every record waits for the next one to find the same: a host
    /// clock stepped forward past the retention makes everything look old, and deleted records do
    /// not come back (RS-13). Pruning by hand (`prune_audit`) is the user's word and does not wait.
    pub(super) fn prune_audit_if_due(&self) -> Result<u64, StoreError> {
        let now = self.clock.now_ms();
        let last = self.last_audit_prune.load(Ordering::Relaxed);
        if last != NEVER && now >= last && now - last < PRUNE_EVERY_MS {
            return Ok(0);
        }
        let keep_days = self.audit_retention_days()?;
        let cutoff = sql_ts(now.saturating_sub(u64::from(keep_days).saturating_mul(DAY_MS)));
        if self.prune_would_empty_audit(cutoff)? {
            if !self.audit_wipe_deferred.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    keep_days,
                    "every audit record is past its retention (a clock step?); deleting them only if the next pass agrees"
                );
                self.last_audit_prune.store(now, Ordering::Relaxed);
                return Ok(0);
            }
        } else {
            self.audit_wipe_deferred.store(false, Ordering::Relaxed);
        }
        let pruned = self.prune_audit(keep_days)?;
        // Only a pass that worked counts, so a failed one is tried again at the next sweep.
        self.last_audit_prune.store(now, Ordering::Relaxed);
        self.audit_wipe_deferred.store(false, Ordering::Relaxed);
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

    /// Folds the write-ahead log into the database and truncates its file, so a burst of writes
    /// that a long reader held back from checkpointing does not leave the log at its peak size.
    /// A reader still open at this moment leaves the log alone until the next sweep.
    ///
    /// Runs under the writer's lock without waiting: a plain checkpoint first (it folds in what
    /// no reader holds back), then the truncation, which a reader or another writer refuses at
    /// once instead of making every decision wait out the busy timeout (RS-16).
    ///
    /// # Errors
    /// A database error.
    pub fn checkpoint_wal(&self) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        let normal: i64 = conn.pragma_query_value(None, "busy_timeout", |row| row.get(0))?;
        conn.busy_timeout(Duration::ZERO)?;
        let result = ["PASSIVE", "TRUNCATE"].into_iter().try_for_each(|mode| {
            // The row says whether the checkpoint finished; a refused one is retried at the next sweep.
            conn.query_row(&format!("PRAGMA wal_checkpoint({mode})"), [], |_| Ok(()))
        });
        let restored = conn.busy_timeout(Duration::from_millis(u64::try_from(normal).unwrap_or(0)));
        result.and(restored)?;
        Ok(())
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
            free_pages(&conn, SHRINK_STEP_PAGES)?;
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
    if free == 0 {
        return Ok(None);
    }
    let worth = free >= min_free_bytes
        || free.saturating_mul(100) >= total.saturating_mul(SHRINK_MIN_FREE_PERCENT);
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
