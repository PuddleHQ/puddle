// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recording the audit log: connections, workspace operations, the sweep, and the audit queries.

use std::collections::HashMap;

use puddle_types::{ConnectionEvent, ConnectionLog, ConnectionOrigin, Event, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::audit::{
    AuditOutcome, AuditRecord, ConnectionRecord, PendingExpiryReason, RuleDeleteReason, RuleWire,
    actor_str,
};
use crate::error::StoreError;
use crate::rule::Actor;
use crate::store::AllUsers;
use crate::users::UserId;

use super::rows::{RULE_COLUMNS, corrupt, expire_rows, raw_rule, rule_from_raw, stored_ts};
use super::{
    AuditCursor, AuditFilter, Flushed, NewWorkspaceOp, Store, SweepReport, WorkspaceDeletion, lock,
    sql_ts, write_tx,
};

impl Store {
    /// Writes a `connection` record (R-24), subject to the per-workspace limit (R-26).
    ///
    /// # Errors
    /// A database or audit error.
    pub fn record_connection(&self, event: &ConnectionEvent) -> Result<(), StoreError> {
        let now = self.clock.now_ms();
        let limit = self.limits.connection_records_per_second;
        let (admitted, summary) = match &event.workspace {
            Some(workspace) => lock(&self.workspaces)
                .entry(workspace.clone())
                .or_default()
                .connections
                .admit(now, limit),
            None => lock(&self.puddle_connections).admit(now, limit),
        };
        if !admitted && summary.is_none() {
            return Ok(());
        }
        let written = (|| {
            let mut conn = lock(&self.conn);
            let tx = write_tx(&mut conn)?;
            let head = audit_head(&tx)?;
            if let Some((ts, count)) = summary {
                let record =
                    ConnectionRecord::suppressed_summary(ts, event.workspace.as_ref(), count);
                append(&tx, &AuditRecord::Connection(record))?;
            }
            if admitted {
                let record = ConnectionRecord::from_event(now, event);
                append(&tx, &AuditRecord::Connection(record))?;
            }
            self.commit(tx, head, Vec::new())
        })();
        if let (Err(_), Some((ts, count))) = (&written, summary) {
            self.put_back(vec![Flushed(event.workspace.clone(), ts, count)]);
        }
        written
    }

    /// Writes a `workspace_op` record: a workspace operation or a change to the identities and
    /// repository lists a workspace uses. The host calls it once the operation has ended, so a
    /// failed write costs the record and nothing else; callers log it and go on.
    ///
    /// # Errors
    /// A database or audit error.
    pub fn record_workspace_op(&self, op: &NewWorkspaceOp) -> Result<(), StoreError> {
        self.change(|tx, now, _fx| {
            append_by(
                tx,
                &AuditRecord::WorkspaceOp {
                    ts: now,
                    op: op.op,
                    workspace_id: op.workspace.as_ref().map(|w| w.as_str().to_owned()),
                    outcome: op.outcome,
                    actor: actor_str(op.actor),
                    detail: op.detail.clone(),
                },
                op.user,
            )
        })
    }

    /// Writes a `workspace_op` record for an operation that has already happened. A record that
    /// cannot be written is logged and dropped: the operation stands, and an error here would
    /// read as "not done". Returns whether the record was written.
    pub fn note_workspace_op(&self, op: &NewWorkspaceOp) -> bool {
        match self.record_workspace_op(op) {
            Ok(()) => true,
            Err(err) => {
                tracing::warn!(op = ?op.op, error = %err, "workspace operation not written to the audit log");
                false
            }
        }
    }

    /// Removes a deleted workspace's rules and environment and expires its open rows, in one
    /// transaction (R-21).
    /// Call it from the transaction-equivalent step of workspace deletion, not on stop.
    ///
    /// # Errors
    /// A database or audit error; nothing changes then.
    pub fn delete_workspace(
        &self,
        workspace: &WorkspaceId,
    ) -> Result<WorkspaceDeletion, StoreError> {
        let deletion = self.change(|tx, now, fx| {
            let mut stmt = tx.prepare(&format!(
                "SELECT {RULE_COLUMNS} FROM rules WHERE workspace_id = ?1 ORDER BY id"
            ))?;
            let rules = stmt
                .query_map([workspace.as_str()], raw_rule)?
                .map(|raw| rule_from_raw(&raw?))
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            tx.execute(
                "DELETE FROM rules WHERE workspace_id = ?1",
                [workspace.as_str()],
            )?;
            for rule in &rules {
                append(
                    tx,
                    &AuditRecord::RuleDeleted {
                        ts: now,
                        rule: RuleWire::from(rule),
                        reason: RuleDeleteReason::WorkspaceDeleted,
                        actor: actor_str(Actor::System),
                    },
                )?;
            }
            let switched = tx.execute(
                "DELETE FROM rule_set_switches WHERE workspace_id = ?1",
                [workspace.as_str()],
            )? + tx.execute(
                "DELETE FROM system_reasons WHERE workspace_id = ?1",
                [workspace.as_str()],
            )?;
            let expired = expire_rows(
                tx,
                "workspace_id = ?2",
                &workspace.as_str(),
                now,
                PendingExpiryReason::WorkspaceDeleted,
                fx,
            )?;
            if !rules.is_empty() || switched > 0 {
                fx.push(Event::RulesChanged {});
            }
            // Its identities and repository table go with it; the identities stay.
            if super::identities::delete_workspace_rows(tx, workspace)? {
                fx.push(Event::WorkspaceGitChanged {
                    workspace: workspace.clone(),
                });
            }
            // Its variables and stand-ins go with it; the global ones stay.
            let (secrets, had_env) = super::environment::delete_workspace_rows(tx, workspace)?;
            if had_env {
                fx.push(Event::WorkspaceEnvChanged {
                    workspace: workspace.clone(),
                });
            }
            super::shell_init::delete_workspace_rows(tx, workspace)?;
            // Its pinned forwards go with it.
            super::ports::delete_workspace_rows(tx, workspace)?;
            super::interception::delete_workspace_rows(tx, workspace)?;
            super::learned::delete_workspace_rows(tx, workspace)?;
            super::request_rules::delete_workspace_rows(tx, workspace, now, fx)?;
            super::ssh_agent::delete_workspace_rows(tx, workspace)?;
            Ok(WorkspaceDeletion {
                rules_deleted: rules.len() as u64,
                pending_expired: expired,
                secrets,
            })
        })?;
        lock(&self.workspaces).remove(workspace);
        tracing::info!(workspace = %workspace, rules = deletion.rules_deleted, "workspace rules removed");
        Ok(deletion)
    }

    /// One sweeper pass (R-19, R-22), each step its own short transaction.
    ///
    /// # Errors
    /// The first failing step's error; earlier steps stay committed.
    pub fn sweep(&self) -> Result<SweepReport, StoreError> {
        let rules_expired = self.change(|tx, now, fx| {
            let mut stmt = tx.prepare(&format!(
                "SELECT {RULE_COLUMNS} FROM rules
                 WHERE expires_at IS NOT NULL AND expires_at <= ?1 ORDER BY id"
            ))?;
            let rules = stmt
                .query_map([sql_ts(now)], raw_rule)?
                .map(|raw| rule_from_raw(&raw?))
                .collect::<Result<Vec<_>, _>>()?;
            drop(stmt);
            for rule in &rules {
                tx.execute("DELETE FROM rules WHERE id = ?1", [rule.id.0])?;
                append_for(
                    tx,
                    &AuditRecord::RuleExpired {
                        ts: now,
                        rule: RuleWire::from(rule),
                    },
                    rule.user,
                )?;
            }
            if !rules.is_empty() {
                fx.push(Event::RulesChanged {});
            }
            Ok(rules.len() as u64)
        })?;
        self.flush_limits()?;
        let waiting_records_merged = self.merge_waiting_connections()?;
        let audit_records_trimmed = self.trim_audit()?;
        Ok(SweepReport {
            rules_expired,
            audit_records_trimmed,
            waiting_records_merged,
        })
    }

    /// Merges each workspace's oldest `connection` records of waiting requests past the cap into
    /// one `merged` summary, so a flood of requests nobody has answered cannot fill the audit
    /// and push out real history. Nothing is refused; other records are never touched.
    fn merge_waiting_connections(&self) -> Result<u64, StoreError> {
        let cap = i64::try_from(self.limits.max_waiting_connection_records).unwrap_or(i64::MAX);
        let mut conn = lock(&self.conn);
        let tx = write_tx(&mut conn)?;
        let head = audit_head(&tx)?;
        let over: Vec<(String, i64)> = {
            let mut stmt = tx.prepare(
                "SELECT workspace_id, count(*) FROM audit
                 WHERE type = 'connection' AND outcome = 'pending' AND workspace_id IS NOT NULL
                 GROUP BY workspace_id HAVING count(*) > ?1",
            )?;
            stmt.query_map([cap], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<_, _>>()?
        };
        let mut merged = 0u64;
        for (workspace, count) in over {
            let name = WorkspaceId::new(&workspace)
                .map_err(|_| corrupt("audit", 0, "invalid workspace name"))?;
            let mut stmt = tx.prepare(
                "DELETE FROM audit WHERE id IN (
                     SELECT id FROM audit
                     WHERE type = 'connection' AND outcome = 'pending' AND workspace_id = ?1
                     ORDER BY id LIMIT ?2)
                 RETURNING ts",
            )?;
            let mut newest = 0;
            let mut deleted = 0u64;
            for ts in stmt.query_map(params![workspace, count - cap], |row| row.get::<_, i64>(0))? {
                newest = newest.max(ts?);
                deleted += 1;
            }
            drop(stmt);
            let ts = stored_ts("audit", 0, newest)?;
            append(
                &tx,
                &AuditRecord::Connection(ConnectionRecord::merged_summary(ts, &name, deleted)),
            )?;
            merged += deleted;
        }
        if merged == 0 {
            return Ok(0);
        }
        self.commit(tx, head, Vec::new())?;
        tracing::info!(
            merged,
            "records of waiting requests merged to bound the audit"
        );
        Ok(merged)
    }

    /// Writes the summaries of `connection` records over the limit that are due. They are taken
    /// out of memory first; when the write fails they are put back, so the next sweep writes them.
    fn flush_limits(&self) -> Result<(), StoreError> {
        let now = self.clock.now_ms();
        let mut records = Vec::new();
        let mut taken = Vec::new();
        for (workspace, state) in lock(&self.workspaces).iter_mut() {
            if let Some((ts, count)) = state.connections.roll(now) {
                let record = ConnectionRecord::suppressed_summary(ts, Some(workspace), count);
                records.push(AuditRecord::Connection(record));
                taken.push(Flushed(Some(workspace.clone()), ts, count));
            }
        }
        if let Some((ts, count)) = lock(&self.puddle_connections).roll(now) {
            let record = ConnectionRecord::suppressed_summary(ts, None, count);
            records.push(AuditRecord::Connection(record));
            taken.push(Flushed(None, ts, count));
        }
        if records.is_empty() {
            return Ok(());
        }
        let written = (|| {
            let mut conn = lock(&self.conn);
            let tx = write_tx(&mut conn)?;
            let head = audit_head(&tx)?;
            for record in &records {
                append(&tx, record)?;
            }
            self.commit(tx, head, Vec::new())
        })();
        if written.is_err() {
            self.put_back(taken);
        }
        written
    }

    /// Returns counts taken for an audit write that failed.
    fn put_back(&self, taken: Vec<Flushed>) {
        let mut workspaces = lock(&self.workspaces);
        for item in taken {
            match item {
                Flushed(Some(workspace), ts, count) => {
                    if let Some(state) = workspaces.get_mut(&workspace) {
                        state.connections.owe(ts, count);
                    }
                }
                Flushed(None, ts, count) => {
                    lock(&self.puddle_connections).owe(ts, count);
                }
            }
        }
    }

    /// Deletes the oldest audit records while the audit is over its cap, then records the trim.
    fn trim_audit(&self) -> Result<u64, StoreError> {
        const CHUNK: i64 = 5000;
        let mut deleted = 0u64;
        if self.audit_bytes()? <= self.limits.audit_max_bytes {
            return Ok(0);
        }
        loop {
            let mut conn = lock(&self.conn);
            let tx = write_tx(&mut conn)?;
            let total = audit_bytes(&tx)?;
            let Some(excess) = total.checked_sub(self.limits.audit_trim_to_bytes) else {
                break;
            };
            if excess == 0 {
                break;
            }
            let mut stmt = tx
                .prepare("SELECT id, length(CAST(line AS BLOB)) FROM audit ORDER BY id LIMIT ?1")?;
            let mut freed = 0u64;
            let mut cutoff = None;
            for item in stmt.query_map([CHUNK], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
            })? {
                let (id, len) = item?;
                freed += u64::try_from(len).unwrap_or(0);
                cutoff = Some(id);
                if freed >= excess {
                    break;
                }
            }
            drop(stmt);
            let Some(cutoff) = cutoff else { break };
            deleted += u64::try_from(tx.execute("DELETE FROM audit WHERE id <= ?1", [cutoff])?)
                .unwrap_or(0);
            tx.commit()?;
        }
        let mut conn = lock(&self.conn);
        let tx = write_tx(&mut conn)?;
        let head = audit_head(&tx)?;
        let oldest: Option<i64> = tx
            .query_row("SELECT ts FROM audit ORDER BY id LIMIT 1", [], |row| {
                row.get(0)
            })
            .optional()?;
        append(
            &tx,
            &AuditRecord::AuditTrimmed {
                ts: self.clock.now_ms(),
                deleted_records: deleted,
                oldest_ts_kept: oldest.and_then(|ts| u64::try_from(ts).ok()),
            },
        )?;
        self.commit(tx, head, Vec::new())?;
        tracing::info!(deleted, "audit trimmed to its size cap");
        Ok(deleted)
    }

    /// Total bytes of audit lines.
    ///
    /// # Errors
    /// A database error.
    pub fn audit_bytes(&self) -> Result<u64, StoreError> {
        audit_bytes(&lock(&self.conn))
    }

    /// Audit records after id `after`, oldest first, as `(id, JSONL line)`.
    ///
    /// # Errors
    /// A database error.
    pub fn audit_lines(&self, after: i64, limit: u32) -> Result<Vec<(i64, String)>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt =
            conn.prepare("SELECT id, line FROM audit WHERE id > ?1 ORDER BY id LIMIT ?2")?;
        let rows = stmt.query_map(params![after, limit], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

impl Store {
    /// A page of up to `limit` audit records matching `filter`, as `(id, JSONL line)`: oldest
    /// first for [`AuditCursor::After`], newest first for [`AuditCursor::Before`].
    ///
    /// Filters run on indexed columns (`workspace_id`, `type`, `outcome`, `ts`) and the stored
    /// `host` and `search` text; no JSON is parsed. Values are bound, never spliced into the SQL.
    ///
    /// # Errors
    /// A database error.
    pub fn audit_query_every_user(
        &self,
        _: &AllUsers,
        filter: &AuditFilter,
        cursor: AuditCursor,
        limit: u32,
    ) -> Result<Vec<(i64, String)>, StoreError> {
        self.audit_query_where(filter, cursor, limit)
    }

    fn audit_query_where(
        &self,
        filter: &AuditFilter,
        cursor: AuditCursor,
        limit: u32,
    ) -> Result<Vec<(i64, String)>, StoreError> {
        let (sql, args) = audit_query_sql(filter, cursor, limit);
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |row| {
            Ok((row.get(0)?, row.get(1)?))
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// [`Store::audit_query_every_user`] kept to `user`'s records, whatever `filter.user` says.
    ///
    /// # Errors
    /// A database error.
    pub fn audit_query(
        &self,
        user: UserId,
        filter: &AuditFilter,
        cursor: AuditCursor,
        limit: u32,
    ) -> Result<Vec<(i64, String)>, StoreError> {
        let filter = AuditFilter {
            user: Some(user),
            ..filter.clone()
        };
        self.audit_query_where(&filter, cursor, limit)
    }

    /// Remembers `name` as the current name of `workspace`; audit records written from now on
    /// carry it as `workspace_name`. Call it when a workspace is created or renamed, and for
    /// every workspace at start.
    ///
    /// # Errors
    ///
    /// Fails if the database can't be written.
    pub fn set_workspace_name(
        &self,
        workspace: &WorkspaceId,
        name: &str,
    ) -> Result<(), StoreError> {
        let conn = lock(&self.conn);
        conn.execute(
            "INSERT INTO workspace_names (workspace_id, name) VALUES (?1, ?2)
             ON CONFLICT (workspace_id) DO UPDATE SET name = excluded.name",
            params![workspace.as_str(), name],
        )?;
        Ok(())
    }

    /// [`Store::set_workspace_name`] for a caller that cannot act on a failure: it is logged and
    /// dropped, and the records written meanwhile carry no name (a reader shows the id). Returns
    /// whether the name was written.
    pub fn note_workspace_name(&self, workspace: &WorkspaceId, name: &str) -> bool {
        let written = self.set_workspace_name(workspace, name);
        if let Err(err) = &written {
            tracing::warn!(%workspace, error = %err, "the workspace's name was not written to the store");
        }
        written.is_ok()
    }

    /// The `workspace_name` each of the audit records `ids` was written with; a record written
    /// before the column existed, or with no workspace, has none and is left out.
    ///
    /// # Errors
    ///
    /// Fails if the database can't be read.
    pub fn audit_workspace_names(&self, ids: &[i64]) -> Result<HashMap<i64, String>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare_cached(
            "SELECT workspace_name FROM audit WHERE id = ?1 AND workspace_name IS NOT NULL",
        )?;
        let mut names = HashMap::new();
        for id in ids {
            if let Some(name) = stmt
                .query_row([id], |row| row.get::<_, String>(0))
                .optional()?
            {
                names.insert(*id, name);
            }
        }
        Ok(names)
    }

    /// SQLite's query plan for [`Store::audit_query`] with these arguments, one line per step.
    /// Lets a test check that a filter combination uses an index instead of timing it.
    ///
    /// # Errors
    ///
    /// Fails if the database can't be read.
    pub fn audit_query_plan(
        &self,
        filter: &AuditFilter,
        cursor: AuditCursor,
        limit: u32,
    ) -> Result<Vec<String>, StoreError> {
        let (sql, args) = audit_query_sql(filter, cursor, limit);
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |row| row.get(3))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// The SQL and bound values for one audit page; every value is bound, never spliced in.
pub(super) fn audit_query_sql(
    filter: &AuditFilter,
    cursor: AuditCursor,
    limit: u32,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut sql = String::from("SELECT id, line FROM audit WHERE 1");
    let mut args: Vec<Value> = Vec::new();
    let mut clause = |text: &str, value: Option<Value>| {
        sql.push_str(" AND ");
        sql.push_str(text);
        args.extend(value);
    };
    match cursor {
        AuditCursor::After(id) => clause("id > ?", Some(Value::Integer(id))),
        AuditCursor::Before(Some(id)) => clause("id < ?", Some(Value::Integer(id))),
        AuditCursor::Before(None) => {}
    }
    if let Some(user) = filter.user {
        // `+` keeps the planner on the indexes the other filters pick: users are few, so this
        // narrows little, and the owner's rows are nearly all of them.
        clause("+user_id = ?", Some(Value::Integer(user.get())));
    }
    if let Some(workspace) = &filter.workspace {
        clause("workspace_id = ?", Some(Value::Text(workspace.to_string())));
    }
    if let Some(kind) = filter.kind {
        clause("type = ?", Some(Value::Text(kind.to_owned())));
    }
    if let Some(outcome) = filter.outcome {
        clause(
            "outcome = ?",
            Some(Value::Text(outcome.as_str().to_owned())),
        );
    }
    match filter.origin {
        None => {}
        // A connection record names a workspace unless Puddle made the connection itself.
        Some(ConnectionOrigin::Puddle) => {
            clause("type = 'connection' AND workspace_id IS NULL", None);
        }
        Some(_) => clause("type = 'connection' AND workspace_id IS NOT NULL", None),
    }
    if let Some(from) = filter.from {
        clause("ts >= ?", Some(Value::Integer(sql_ts(from))));
    }
    if let Some(to) = filter.to {
        clause("ts < ?", Some(Value::Integer(sql_ts(to))));
    }
    if let Some(needle) = &filter.host_contains {
        clause(
            "instr(host, ?) > 0",
            Some(Value::Text(needle.to_lowercase())),
        );
    }
    if let Some(needle) = &filter.q {
        clause(
            "instr(search, ?) > 0",
            Some(Value::Text(needle.to_ascii_lowercase())),
        );
    }
    if let Some(category) = filter.category {
        // The category is one whole line of the search text, never a part of another.
        clause(
            "instr(char(10) || search || char(10), char(10) || ? || char(10)) > 0",
            Some(Value::Text(category.to_owned())),
        );
    }
    sql.push_str(match cursor {
        AuditCursor::After(_) => " ORDER BY id LIMIT ?",
        AuditCursor::Before(_) => " ORDER BY id DESC LIMIT ?",
    });
    args.push(Value::Integer(i64::from(limit)));
    (sql, args)
}

/// The proxy's [`ConnectionLog`]: each event becomes a `connection` record through
/// [`Store::record_connection`]. A failed write is logged and dropped; the connection it describes
/// has already been handled.
impl ConnectionLog for Store {
    fn record(&self, event: &ConnectionEvent) {
        if let Err(err) = self.record_connection(event) {
            tracing::warn!(origin = %event.origin, error = %err, "connection record not written");
        }
    }
}

pub(super) fn audit_bytes(conn: &Connection) -> Result<u64, StoreError> {
    let bytes: i64 = conn.query_row("SELECT bytes FROM audit_size WHERE id = 1", [], |row| {
        row.get(0)
    })?;
    // A negative size read as 0 would keep the audit-size cap from ever firing.
    u64::try_from(bytes).map_err(|_| corrupt("audit_size", 1, "negative byte count"))
}

/// Appends a record of the workspace's user it names; one that names none is the owner's (the
/// install's own: Puddle's connections, a trim, the built-in sets and System managed).
pub(super) fn append(conn: &Connection, record: &AuditRecord) -> Result<(), StoreError> {
    append_by(conn, record, None)
}

/// Appends a record of `user`: one whose workspace (if it names one) is `user`'s.
pub(super) fn append_for(
    conn: &Connection,
    record: &AuditRecord,
    user: UserId,
) -> Result<(), StoreError> {
    append_by(conn, record, Some(user))
}

pub(super) fn append_by(
    conn: &Connection,
    record: &AuditRecord,
    user: Option<UserId>,
) -> Result<(), StoreError> {
    let line = record.to_line()?;
    conn.execute(
        "INSERT INTO audit (ts, type, workspace_id, host, outcome, line, search, workspace_name,
                            user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7,
                 (SELECT name FROM workspace_names WHERE workspace_id = ?3),
                 coalesce((SELECT user_id FROM workspace_owners WHERE workspace_id = ?3), ?8, 1))",
        params![
            sql_ts(record.ts()),
            record.kind(),
            record.workspace_id(),
            record.host(),
            record.outcome().map(AuditOutcome::as_str),
            line,
            record.search_text(),
            user.map(UserId::get)
        ],
    )?;
    Ok(())
}

/// For each user with records appended after `head`, their newest record's id.
pub(super) fn appended_since(
    conn: &Connection,
    head: i64,
) -> Result<Vec<(UserId, i64)>, StoreError> {
    const NEWEST_PER_USER: &str =
        "SELECT user_id, max(id) FROM audit WHERE id > ?1 GROUP BY user_id ORDER BY max(id)";
    let mut stmt = conn.prepare_cached(NEWEST_PER_USER)?;
    let rows = stmt.query_map([head], |row| {
        Ok((UserId::from_stored(row.get(0)?), row.get(1)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The newest audit record's id (0 for an empty audit). Ids only grow (`AUTOINCREMENT`), so a
/// transaction that appended anything ends with a larger value than it started with.
pub(super) fn audit_head(conn: &Connection) -> Result<i64, StoreError> {
    Ok(
        conn.query_row("SELECT coalesce(max(id), 0) FROM audit", [], |row| {
            row.get(0)
        })?,
    )
}
