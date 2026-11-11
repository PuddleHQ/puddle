// SPDX-License-Identifier: GPL-3.0-or-later
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
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::clock::ManualClock;
    use crate::store::{LearnedLimits, Limits};

    fn ws(text: &str) -> WorkspaceId {
        WorkspaceId::new(text).unwrap()
    }

    fn store() -> (Arc<ManualClock>, Store) {
        let clock = Arc::new(ManualClock::new(1_000));
        let store = Store::open_in_memory(clock.clone(), Limits::default()).unwrap();
        (clock, store)
    }

    #[test]
    fn a_refusal_makes_a_row_and_each_later_one_raises_the_count_and_the_last_time() {
        let (clock, store) = store();
        let first = store.record_rejection(&ws("a"), "pin.example").unwrap();
        assert_eq!(
            first,
            LearnedRow {
                host: "pin.example".into(),
                first_at: 1_000,
                last_at: 1_000,
                count: 1
            }
        );
        clock.advance(5_000);
        let second = store.record_rejection(&ws("a"), "pin.example").unwrap();
        assert_eq!(
            (second.first_at, second.last_at, second.count),
            (1_000, 6_000, 2)
        );
        assert_eq!(
            store.learned_history(&ws("a")).unwrap(),
            vec![second.clone()]
        );
        assert_eq!(
            store.learned_host(&ws("a"), "pin.example").unwrap(),
            Some(second)
        );
        assert_eq!(store.learned_host(&ws("b"), "pin.example").unwrap(), None);
        assert_eq!(store.learned_history(&ws("b")).unwrap(), []);
    }

    #[test]
    fn the_history_lists_the_most_recent_refusal_first() {
        let (clock, store) = store();
        store.record_rejection(&ws("a"), "old.example").unwrap();
        clock.advance(10);
        store.record_rejection(&ws("a"), "new.example").unwrap();
        clock.advance(10);
        store.record_rejection(&ws("a"), "old.example").unwrap();
        let hosts: Vec<_> = store
            .learned_history(&ws("a"))
            .unwrap()
            .into_iter()
            .map(|r| (r.host, r.count))
            .collect();
        assert_eq!(
            hosts,
            [("old.example".to_owned(), 2), ("new.example".to_owned(), 1)]
        );
    }

    #[test]
    fn past_the_cap_the_host_refused_longest_ago_makes_room() {
        let (clock, store) = store();
        store
            .set_learned_limits(LearnedLimits {
                ttl_secs: 3600,
                cap: 2,
            })
            .unwrap();
        for host in ["a.example", "b.example", "c.example"] {
            store.record_rejection(&ws("w"), host).unwrap();
            clock.advance(1);
        }
        let hosts: Vec<_> = store
            .learned_history(&ws("w"))
            .unwrap()
            .into_iter()
            .map(|r| r.host)
            .collect();
        assert_eq!(hosts, ["c.example", "b.example"]);
        store.record_rejection(&ws("other"), "a.example").unwrap();
        assert_eq!(store.learned_history(&ws("other")).unwrap().len(), 1);
    }

    #[test]
    fn dropping_a_host_removes_only_its_row_and_a_deleted_workspace_loses_all_of_them() {
        let (_clock, store) = store();
        store.record_rejection(&ws("a"), "x.example").unwrap();
        store.record_rejection(&ws("a"), "y.example").unwrap();
        assert!(store.drop_learned_host(&ws("a"), "x.example").unwrap());
        assert!(!store.drop_learned_host(&ws("a"), "x.example").unwrap());
        assert_eq!(store.learned_history(&ws("a")).unwrap().len(), 1);
        let mut conn = lock(&store.conn);
        let tx = conn.transaction().unwrap();
        delete_workspace_rows(&tx, &ws("a")).unwrap();
        tx.commit().unwrap();
        drop(conn);
        assert_eq!(store.learned_history(&ws("a")).unwrap(), []);
    }
}
