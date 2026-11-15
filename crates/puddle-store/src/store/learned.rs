// SPDX-License-Identifier: AGPL-3.0-or-later
//! The history of the hosts a workspace's tools refused Puddle's certificate for: when each was
//! first and last refused and how many times. The pass-through itself lives in memory in the
//! running workspace; this is what survives its expiry and a restart.

use puddle_types::WorkspaceId;
use rusqlite::{OptionalExtension, Transaction, params};

use super::{Store, lock};
use crate::error::StoreError;

/// One host a workspace's tools refused Puddle's certificate for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearnedRow {
    /// The host name, lower case.
    pub host: String,
    /// When it was first refused, milliseconds since the epoch.
    pub first_at: u64,
    /// When it was last refused, milliseconds since the epoch.
    pub last_at: u64,
    /// How many times Puddle decrypted it and the tool refused (each refusal after Puddle
    /// stopped passing it through counts once).
    pub count: u32,
}

impl Store {
    /// Notes that a tool in `workspace` refused Puddle's certificate for `host` now: the first
    /// refusal makes the row, each later one raises the count. A workspace keeps at most the
    /// learned-host cap of rows; the one refused longest ago makes room.
    ///
    /// # Errors
    /// A database error; nothing changes then.
    pub fn record_rejection(
        &self,
        workspace: &WorkspaceId,
        host: &str,
    ) -> Result<LearnedRow, StoreError> {
        let cap = self.learned_limits()?.cap;
        let now = self.clock.now_ms();
        let mut conn = lock(&self.conn);
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO learned_hosts (workspace_id, host, first_at, last_at, count)
             VALUES (?1, ?2, ?3, ?3, 1)
             ON CONFLICT (workspace_id, host)
             DO UPDATE SET last_at = ?3, count = count + 1",
            params![
                workspace.as_str(),
                host,
                i64::try_from(now).unwrap_or(i64::MAX)
            ],
        )?;
        tx.execute(
            "DELETE FROM learned_hosts WHERE workspace_id = ?1 AND host NOT IN (
                 SELECT host FROM learned_hosts WHERE workspace_id = ?1
                 ORDER BY last_at DESC, host LIMIT ?2)",
            params![workspace.as_str(), cap],
        )?;
        let row = tx.query_row(
            "SELECT host, first_at, last_at, count FROM learned_hosts
             WHERE workspace_id = ?1 AND host = ?2",
            params![workspace.as_str(), host],
            row_of,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// The hosts `workspace`'s tools refused Puddle's certificate for, the most recent first.
    ///
    /// # Errors
    /// A database error.
    pub fn learned_history(&self, workspace: &WorkspaceId) -> Result<Vec<LearnedRow>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(
            "SELECT host, first_at, last_at, count FROM learned_hosts
             WHERE workspace_id = ?1 ORDER BY last_at DESC, host",
        )?;
        Ok(stmt
            .query_map([workspace.as_str()], row_of)?
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Forgets the history of `host` in `workspace`. Whether there was one.
    ///
    /// # Errors
    /// A database error.
    pub fn drop_learned_host(
        &self,
        workspace: &WorkspaceId,
        host: &str,
    ) -> Result<bool, StoreError> {
        let conn = lock(&self.conn);
        Ok(conn.execute(
            "DELETE FROM learned_hosts WHERE workspace_id = ?1 AND host = ?2",
            params![workspace.as_str(), host],
        )? > 0)
    }

    /// The row for `host` in `workspace`, if it was ever refused.
    ///
    /// # Errors
    /// A database error.
    pub fn learned_host(
        &self,
        workspace: &WorkspaceId,
        host: &str,
    ) -> Result<Option<LearnedRow>, StoreError> {
        let conn = lock(&self.conn);
        Ok(conn
            .query_row(
                "SELECT host, first_at, last_at, count FROM learned_hosts
                 WHERE workspace_id = ?1 AND host = ?2",
                params![workspace.as_str(), host],
                row_of,
            )
            .optional()?)
    }
}

fn row_of(r: &rusqlite::Row<'_>) -> rusqlite::Result<LearnedRow> {
    Ok(LearnedRow {
        host: r.get(0)?,
        first_at: u64::try_from(r.get::<_, i64>(1)?).unwrap_or_default(),
        last_at: u64::try_from(r.get::<_, i64>(2)?).unwrap_or_default(),
        count: r.get(3)?,
    })
}

/// Removes what a deleted workspace held.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceId,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM learned_hosts WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
