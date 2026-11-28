// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recording the audit log: connections, workspace operations, the sweep, and the audit queries.

use std::collections::HashMap;

use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionLog, ConnectionOrigin, ConnectionReason, Event,
    WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::audit::{AuditOutcome, AuditRecord, ConnectionRecord, RuleWire, actor_str};
use crate::error::StoreError;
use crate::store::AllUsers;
use crate::users::UserId;

use super::rows::{RULE_COLUMNS, corrupt, raw_rule, rule_from_raw, stored_ts};
use super::{
    AuditCursor, AuditFilter, Flushed, NewWorkspaceOp, Store, SweepReport, lock, sql_ts, write_tx,
};

impl Store {
    /// Writes a `connection` record (R-24), subject to the per-workspace limit (R-26).
    ///
    /// # Errors
    /// A database or audit error.
    pub fn record_connection(&self, event: &ConnectionEvent) -> Result<(), StoreError> {
        let now = self.clock.now_ms();
        let monotonic = self.clock.monotonic_ms();
        let limit = self.limits.connection_records_per_second;
        let (admitted, summary) = match &event.workspace {
            Some(workspace) => {
                let mut workspaces = lock(&self.workspaces);
                let state = workspaces.entry(workspace.clone()).or_default();
                // Counted before the record limit, so a flood is still counted in full. Puddle's
                // own requests and certificate fetches have another reason and are not counted.
                if matches!(event.decision, ConnectionDecision::Allow)
                    && matches!(event.reason, ConnectionReason::Rule)
                {
                    state.allowed.bump(now);
                }
                state.connections.admit(now, monotonic, limit)
            }
            None => lock(&self.puddle_connections).admit(now, monotonic, limit),
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
                append(&tx, &AuditRecord::connection(record))?;
            }
            if admitted {
                let record = ConnectionRecord::from_event(now, event);
                append(&tx, &AuditRecord::connection(record))?;
            }
            self.commit(tx, head, Vec::new())
        })();
        if let (Err(_), Some((ts, count))) = (&written, summary) {
            self.put_back(vec![Flushed(event.workspace.clone(), ts, count)]);
        }
        written
    }

    /// How many connections of `user`'s workspaces a rule allowed since the start of the UTC day,
    /// and that start (epoch ms). Counted in memory, so a restart starts again at zero.
    pub fn allowed_today(&self, user: UserId) -> (u64, u64) {
        let now = self.clock.now_ms();
        let index = self.snapshot();
        let count = lock(&self.workspaces)
            .iter()
            .filter(|(workspace, _)| index.owner_of(workspace) == Some(user))
            .map(|(_, state)| state.allowed.on(now))
            .sum();
        (count, now / super::DAY_MS * super::DAY_MS)
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

    /// Whether the database file, less its free pages, is over [`super::Limits::audit_warn_bytes`].
    ///
    /// # Errors
    /// A database error.
    pub fn audit_over_warn_size(&self) -> Result<bool, StoreError> {
        Ok(self.audit_file_in_use_bytes()? > self.limits.audit_warn_bytes)
    }

    /// The database file's size less its free pages (indexes and other tables included).
    ///
    /// # Errors
    /// A database error.
    pub fn audit_file_in_use_bytes(&self) -> Result<u64, StoreError> {
        super::audit_retention::file_in_use_bytes(&lock(&self.conn))
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
        });
        // Each step runs whatever an earlier one did, so a step that fails every pass cannot
        // starve the prune, the size warning or the shrink behind it (RS-12); the first error
        // is the one returned.
        let mut first_error = None;
        let rules_expired = settle(rules_expired, &mut first_error);
        settle(self.flush_limits(), &mut first_error);
        let waiting_records_merged = settle(self.merge_waiting_connections(), &mut first_error);
        let devices_removed = settle(self.prune_stopped_devices(), &mut first_error);
        let audit_records_pruned = settle(self.prune_audit_if_due(), &mut first_error);
        // Before the shrink, so that a shrink that fails (a full disk) never hides the warning.
        let audit_over_warn_size = settle(self.audit_over_warn_size(), &mut first_error);
        let audit_file_shrunk_bytes = self.shrink_audit_file().unwrap_or_else(|err| {
            tracing::warn!(%err, "the audit file's free space could not be given back; the next sweep tries again");
            0
        });
        if let Err(err) = self.checkpoint_wal() {
            tracing::warn!(%err, "the write-ahead log could not be truncated; the next sweep tries again");
        }
        if let Some(err) = first_error {
            return Err(err);
        }
        Ok(SweepReport {
            rules_expired,
            audit_records_pruned,
            audit_file_shrunk_bytes,
            audit_over_warn_size,
            waiting_records_merged,
            devices_removed,
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
                &AuditRecord::connection(ConnectionRecord::merged_summary(ts, &name, deleted)),
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
        let monotonic = self.clock.monotonic_ms();
        let mut records = Vec::new();
        let mut taken = Vec::new();
        for (workspace, state) in lock(&self.workspaces).iter_mut() {
            if let Some((ts, count)) = state.connections.roll(monotonic) {
                let record = ConnectionRecord::suppressed_summary(ts, Some(workspace), count);
                records.push(AuditRecord::connection(record));
                taken.push(Flushed(Some(workspace.clone()), ts, count));
            }
        }
        let rolled = lock(&self.puddle_connections).roll(monotonic);
        if let Some((ts, count)) = rolled {
            let record = ConnectionRecord::suppressed_summary(ts, None, count);
            records.push(AuditRecord::connection(record));
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

    /// Total bytes of audit lines.
    ///
    /// # Errors
    /// A database error.
    pub fn audit_bytes(&self) -> Result<u64, StoreError> {
        audit_bytes(&lock(&self.conn))
    }

    /// Audit records of every user after id `after`, oldest first, as `(id, JSONL line)`, for a
    /// test.
    ///
    /// # Errors
    /// A database error.
    #[cfg(any(test, feature = "testing"))]
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
        let conn = self.audit_read();
        let (sql, args) =
            audit_query_sql(filter, cursor, limit, time_window_is_narrow(&conn, filter)?);
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

    /// The `workspace_name` each of `user`'s audit records `ids` was written with; a record of
    /// another user, written before the column existed, or with no workspace, has none and is
    /// left out.
    ///
    /// # Errors
    ///
    /// Fails if the database can't be read.
    pub fn audit_workspace_names(
        &self,
        user: UserId,
        ids: &[i64],
    ) -> Result<HashMap<i64, String>, StoreError> {
        let conn = self.audit_read();
        let mut stmt = conn.prepare_cached(
            "SELECT workspace_name FROM audit
              WHERE id = ?1 AND user_id = ?2 AND workspace_name IS NOT NULL",
        )?;
        let mut names = HashMap::new();
        for id in ids {
            if let Some(name) = stmt
                .query_row(params![id, user.get()], |row| row.get::<_, String>(0))
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
        let conn = self.audit_read();
        let (sql, args) =
            audit_query_sql(filter, cursor, limit, time_window_is_narrow(&conn, filter)?);
        let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args), |row| row.get(3))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// How many SQLite virtual-machine steps [`Store::audit_query_every_user`] takes for these
    /// arguments, rows included. A step count does not move with the machine's load, so a test can
    /// put a bar on the work a query does (a per-row rescan multiplies it) instead of on a clock.
    ///
    /// # Errors
    ///
    /// Fails if the database can't be read.
    pub fn audit_query_steps(
        &self,
        _: &AllUsers,
        filter: &AuditFilter,
        cursor: AuditCursor,
        limit: u32,
    ) -> Result<i32, StoreError> {
        let conn = self.audit_read();
        let (sql, args) =
            audit_query_sql(filter, cursor, limit, time_window_is_narrow(&conn, filter)?);
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(args))?;
        while rows.next()?.is_some() {}
        drop(rows);
        Ok(stmt.get_status(rusqlite::StatementStatus::VmStep))
    }
}

/// A time window holding fewer records than this is read through the `audit_ts` index and sorted
/// by id; any wider one is read in id order, newest first, which finds a page quickly when the
/// records it wants are common.
const NARROW_WINDOW_RECORDS: i64 = 10_000;

/// Whether `filter`'s `from`/`to` window holds under [`NARROW_WINDOW_RECORDS`] records. SQLite
/// walks `id` for a page whatever the window, which reads the whole table when few records fall
/// in it (732 ms at 5 million rows for an hour that had none); a probe of at most that many
/// index entries tells the two cases apart for the cost of reading them.
fn time_window_is_narrow(conn: &Connection, filter: &AuditFilter) -> Result<bool, StoreError> {
    if filter.from.is_none() && filter.to.is_none() {
        return Ok(false);
    }
    let from = filter.from.map_or(0, sql_ts);
    let to = filter.to.map_or(i64::MAX, sql_ts);
    let found: i64 = conn.query_row(
        "SELECT count(*) FROM (SELECT 1 FROM audit INDEXED BY audit_ts
                                WHERE ts >= ?1 AND ts < ?2 LIMIT ?3)",
        params![from, to, NARROW_WINDOW_RECORDS],
        |row| row.get(0),
    )?;
    Ok(found < NARROW_WINDOW_RECORDS)
}

/// The SQL and bound values for one audit page; every value is bound, never spliced in.
/// `by_time` reads the page through the `audit_ts` index (see [`time_window_is_narrow`]).
pub(super) fn audit_query_sql(
    filter: &AuditFilter,
    cursor: AuditCursor,
    limit: u32,
    by_time: bool,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut sql = String::from(if by_time {
        "SELECT id, line FROM audit INDEXED BY audit_ts WHERE 1"
    } else {
        "SELECT id, line FROM audit WHERE 1"
    });
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
            Some(Value::Text(needle.to_ascii_lowercase())),
        );
    }
    if let Some(needle) = &filter.q {
        clause(
            "instr(search, ?) > 0",
            Some(Value::Text(needle.to_ascii_lowercase())),
        );
    }
    if let Some(category) = filter.category {
        clause("category = ?", Some(Value::Text(category.to_owned())));
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

/// The user id of a record whose workspace has no owner row: no user has it, so no user reads the
/// record, while the every-user reads still see it.
pub(super) const NOBODY: i64 = 0;

pub(super) fn append_by(
    conn: &Connection,
    record: &AuditRecord,
    user: Option<UserId>,
) -> Result<(), StoreError> {
    let line = record.to_line()?;
    conn.execute(
        "INSERT INTO audit (ts, type, workspace_id, host, outcome, line, search, category,
                            workspace_name, user_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?10,
                 (SELECT name FROM workspace_names WHERE workspace_id = ?3),
                 coalesce((SELECT user_id FROM workspace_owners WHERE workspace_id = ?3),
                          CASE WHEN ?3 IS NULL THEN coalesce(?8, 1) ELSE ?9 END))",
        params![
            sql_ts(record.ts()),
            record.kind(),
            record.workspace_id(),
            record.host(),
            record.outcome().map(AuditOutcome::as_str),
            line,
            record.search_text(),
            user.map(UserId::get),
            NOBODY,
            record.category()
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

/// A sweep step's value, or its default after it failed; the first failure is kept in `first`.
fn settle<T: Default>(step: Result<T, StoreError>, first: &mut Option<StoreError>) -> T {
    step.unwrap_or_else(|err| {
        tracing::warn!(%err, "a sweep step failed; the next sweep tries again");
        first.get_or_insert(err);
        T::default()
    })
}
