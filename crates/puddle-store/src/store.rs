// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SQLite-backed store: rules, pending requests, audit, sweeper work.
//!
//! Decisions read an in-memory rule index snapshot (rules, rule set entries and switches)
//! without touching SQLite unless nothing matches. Every change commits to SQLite first and then swaps in a new snapshot built inside
//! the same transaction (R-8). Lock order, where two are held: `conn`, then `workspaces`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::Duration;

use puddle_types::{
    ConnectionOrigin, Decision, EgressRequest, Event, EventSink, Host, NullSink, PendingId,
    PendingOutcome, PendingSummary, Policy, PolicyError, RuleId, SuffixAllows, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::audit::{
    AuditOutcome, AuditRecord, ConnectionWindow, PendingWire, RuleDeleteReason, RuleWire,
    WorkspaceOpKind, WorkspaceOpOutcome, actor_str,
};
use crate::clock::Clock;
use crate::engine::{Hit, RuleIndex};
use crate::error::StoreError;
use crate::pattern::{Pattern, registrable_domain};
use crate::pending::{
    Decided, InboxGroup, PatternChoice, PendingRow, PendingState, Resolution, ScopeChoice,
};
use crate::rule::{Actor, NewRule, Rule, Scope};
use crate::schema;
use crate::users::UserId;

mod admin;
mod audit;
mod devices;
mod environment;
#[cfg(test)]
mod failures;
mod identities;
mod interception;
mod learned;
mod ports;
mod request_rules;
mod rows;
mod scoped;
mod sets;
mod shell_init;
mod ssh_agent;
pub(crate) mod users;

use audit::{append, append_for, appended_since, audit_head};
use rows::{
    PENDING_COLUMNS, close_decided_rows, corrupt, decide_row, decide_row_by, insert_rule,
    load_pending, load_rule, load_rules, pending_from_raw, raw_pending, stored_ts,
};

pub use interception::{
    DEFAULT_LEARNED_CAP, DEFAULT_LEARNED_TTL_SECS, InterceptionSettings, LearnedLimits,
    WorkspaceInterception,
};
pub use learned::LearnedRow;
pub use ports::PinnedPort;
pub use request_rules::{MAX_REQUEST_RULES, NewRequestRule, RequestRule};
pub use scoped::UserStore;
pub use sets::{RuleSetEntryInfo, RuleSetInfo, SystemHost, SystemPlan, parse_rule_set};
pub use ssh_agent::{MAX_SSH_AGENT_HOSTS, MAX_SSH_AGENT_KEYS, SshAgentHost};
pub use users::{AllUsers, WorkspaceOwners};

/// Tunable limits. The defaults are the spec's *(default)* values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limits {
    /// Most open pending rows per workspace (R-45). A request for a new host past it gets no
    /// row of its own and is answered like any waiting one; it is listed once room is made.
    pub max_open_rows: u64,
    /// The audit is trimmed when its lines exceed this many bytes (R-26).
    pub audit_max_bytes: u64,
    /// A trim deletes the oldest records until this many bytes remain.
    pub audit_trim_to_bytes: u64,
    /// `connection` records per workspace per second (R-26).
    pub connection_records_per_second: u32,
    /// Most `connection` records written while a request waited (decision `pending`) kept per
    /// workspace; the sweeper merges the oldest ones past it into one summary record (R-26).
    pub max_waiting_connection_records: u64,
}

impl Default for Limits {
    fn default() -> Self {
        const MIB: u64 = 1024 * 1024;
        Self {
            max_open_rows: 1000,
            audit_max_bytes: 256 * MIB,
            audit_trim_to_bytes: 230 * MIB,
            connection_records_per_second: 200,
            max_waiting_connection_records: 5000,
        }
    }
}

/// A workspace operation to record ([`Store::record_workspace_op`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewWorkspaceOp {
    /// What was done.
    pub op: WorkspaceOpKind,
    /// The workspace it was done to; `None` for an identity change that belongs to none.
    pub workspace: Option<WorkspaceId>,
    /// Whose record it is when it names no workspace (`None`: the owner's). One that names a
    /// workspace is that workspace's user's.
    pub user: Option<UserId>,
    /// How it ended.
    pub outcome: WorkspaceOpOutcome,
    /// Who asked.
    pub actor: Actor,
    /// A short plain sentence (why it failed, or what it touched); never a secret.
    pub detail: Option<String>,
}

/// Which audit records to read. Every field left `None` matches everything; the set
/// ones all have to match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditFilter {
    /// Records of this user's workspaces and changes; every user's when `None`.
    pub user: Option<UserId>,
    /// Records about this workspace.
    pub workspace: Option<WorkspaceId>,
    /// Records of this `type` (one of [`AuditRecord::KINDS`]).
    pub kind: Option<&'static str>,
    /// Records with this outcome. Records that have none never match.
    pub outcome: Option<AuditOutcome>,
    /// `connection` records with this origin; other records have none and never match.
    pub origin: Option<ConnectionOrigin>,
    /// Records whose host (or a rule's pattern) contains this text, compared case-folded.
    pub host_contains: Option<String>,
    /// Records whose search text (host or a rule's pattern, method, path, reason; see
    /// [`AuditRecord::search_text`]) contains this text, compared case-folded (ASCII).
    pub q: Option<String>,
    /// `connection` records in this destination category (`company_network`, see
    /// `ConnectionRecord::category`); other records have none and never match.
    pub category: Option<&'static str>,
    /// Records at or after this epoch ms.
    pub from: Option<u64>,
    /// Records before this epoch ms.
    pub to: Option<u64>,
}

/// Where a page of audit records starts and which way it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditCursor {
    /// Oldest first, from the record after this id (for following the log's tail).
    After(i64),
    /// Newest first, from the record before this id, or from the newest (for paging back).
    Before(Option<i64>),
}

/// What one sweep did (R-19, R-22).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SweepReport {
    /// Expired rules deleted.
    pub rules_expired: u64,
    /// Audit records deleted by the size cap.
    pub audit_records_trimmed: u64,
    /// Records of waiting requests merged into summaries by the per-workspace cap.
    pub waiting_records_merged: u64,
}

/// What deleting a workspace removed (R-21).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkspaceDeletion {
    /// Its rules, deleted.
    pub rules_deleted: u64,
    /// Its open pending rows, closed as expired.
    pub pending_expired: u64,
    /// The secrets it had of its own, by name and by the id of the value in the operating
    /// system's credential store (the values are not in the database, so the caller removes them
    /// there).
    pub secrets: Vec<(crate::EnvName, puddle_secrets::StoredId)>,
}

/// Per-workspace in-memory limits state (R-26). Lost on restart, which only resets the limits.
#[derive(Debug, Default)]
struct WorkspaceState {
    connections: ConnectionWindow,
}

#[cfg(test)]
impl Store {
    /// Makes every audit write of `kind` fail, as a full disk would, until
    /// [`Store::audit_writes_work_again`].
    pub(crate) fn fail_audit_writes(&self, kind: &str) {
        lock(&self.conn)
            .execute_batch(&format!(
                "CREATE TRIGGER audit_write_fails BEFORE INSERT ON audit WHEN NEW.type = '{kind}'
                 BEGIN SELECT RAISE(ABORT, 'disk full'); END"
            ))
            .unwrap();
    }

    pub(crate) fn audit_writes_work_again(&self) {
        lock(&self.conn)
            .execute_batch("DROP TRIGGER audit_write_fails")
            .unwrap();
    }

    /// Runs `sql` on the connection, to break something a test wants to see fail: a trigger that
    /// raises, or a table dropped.
    pub(crate) fn break_database(&self, sql: &str) {
        lock(&self.conn).execute_batch(sql).unwrap();
    }

    /// Makes every statement that calls the SQL function `name` fail to prepare.
    pub(crate) fn refuse_sql_function(&self, name: &'static str) {
        use rusqlite::hooks::{AuthAction, Authorization};
        lock(&self.conn)
            .authorizer(Some(
                move |context: rusqlite::hooks::AuthContext<'_>| match context.action {
                    AuthAction::Function { function_name } if function_name == name => {
                        Authorization::Deny
                    }
                    _ => Authorization::Allow,
                },
            ))
            .unwrap();
    }
}

/// A finished second's excess of `connection` records that [`Store::flush_limits`] took out of
/// memory: the workspace (`None` for Puddle's own), its start and count.
struct Flushed(Option<WorkspaceId>, u64, u64);

/// Puddle's rules, pending requests and audit log, in one SQLite database.
pub struct Store {
    conn: Arc<Mutex<Connection>>,
    /// Whether [`Store::take_account_secrets`] has handed the sign-in secrets out.
    secrets_taken: AtomicBool,
    /// Whether [`Store::take_all_users`] has handed the reach across every user out.
    all_users_taken: AtomicBool,
    rules: RwLock<Arc<RuleIndex>>,
    workspaces: Mutex<HashMap<WorkspaceId, WorkspaceState>>,
    /// The connection limit of Puddle's own connections, which have no workspace.
    puddle_connections: Mutex<ConnectionWindow>,
    clock: Arc<dyn Clock>,
    limits: Limits,
    events: Arc<dyn EventSink>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock can't leave SQLite half-written (the transaction rolls
    // back on drop), and the other guarded state is counters, so carry on.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Starts a transaction that holds the write lock from its first statement. Every write here begins
/// with a read (`audit_head`), and a plain `BEGIN` that reads first and writes later fails the moment
/// another connection to the file (the CLI, a second host start) holds the write lock: SQLite does not
/// call the busy handler for a read that must become a write, so the 5 s wait never starts.
pub(crate) fn write_tx(conn: &mut Connection) -> Result<Transaction<'_>, rusqlite::Error> {
    conn.transaction_with_behavior(TransactionBehavior::Immediate)
}

/// Epoch ms as SQLite stores it. Times past year 292 million saturate.
pub(crate) fn sql_ts(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

fn plus(now: u64, duration: Duration) -> u64 {
    now.saturating_add(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
}

impl Store {
    /// Opens (creating if needed) the database at `path` and migrates it.
    ///
    /// # Errors
    /// [`StoreError`] if the file can't be opened, is from a newer Puddle, or holds invalid rows.
    pub fn open(path: &Path, clock: Arc<dyn Clock>, limits: Limits) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "wal")?;
        conn.pragma_update(None, "synchronous", "normal")?;
        Self::init(conn, clock, limits)
    }

    /// An in-memory store, for tests.
    ///
    /// # Errors
    /// [`StoreError`] if SQLite can't create it.
    pub fn open_in_memory(clock: Arc<dyn Clock>, limits: Limits) -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?, clock, limits)
    }

    fn init(
        mut conn: Connection,
        clock: Arc<dyn Clock>,
        limits: Limits,
    ) -> Result<Self, StoreError> {
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", true)?;
        schema::migrate(&mut conn)?;
        sets::note_built_in_changes(&mut conn, clock.now_ms())?;
        let rules = sets::load_index(&conn)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            secrets_taken: AtomicBool::new(false),
            all_users_taken: AtomicBool::new(false),
            rules: RwLock::new(Arc::new(rules)),
            workspaces: Mutex::new(HashMap::new()),
            puddle_connections: Mutex::new(ConnectionWindow::default()),
            clock,
            limits,
            events: Arc::new(NullSink),
        })
    }

    /// Sends the events of every change (pending requests opened, updated and closed, rules
    /// changed, audit records appended) to `sink`, after the change has
    /// committed. Nothing is emitted by default. Events carry ids, not decisions: a client that
    /// misses one refetches.
    #[must_use]
    pub fn with_events(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.events = sink;
        self
    }

    fn snapshot(&self) -> Arc<RuleIndex> {
        Arc::clone(&self.rules.read().unwrap_or_else(PoisonError::into_inner))
    }

    /// Every rule of every user, including expired ones the sweeper hasn't removed yet, and the
    /// entries of the rule sets users made (scope [`Scope::Set`]). For the install's own jobs
    /// (backups, checks); what a user is shown comes from [`Store::rules`].
    #[must_use]
    pub fn rules_every_user(&self, _: &AllUsers) -> Vec<Rule> {
        self.snapshot().rules().to_vec()
    }

    /// Whose rule `id` is, or `None` when there is no such rule.
    #[must_use]
    pub fn rule_owner(&self, id: RuleId) -> Option<UserId> {
        self.snapshot()
            .rules()
            .iter()
            .find(|rule| rule.id == id)
            .map(|rule| rule.user)
    }

    /// The rules of `user`, as [`Store::rules_every_user`] lists them.
    #[must_use]
    pub fn rules(&self, user: UserId) -> Vec<Rule> {
        self.snapshot()
            .rules()
            .iter()
            .filter(|rule| rule.user == user)
            .cloned()
            .collect()
    }

    /// The most open rows kept per workspace (R-45).
    #[must_use]
    pub fn max_open_rows(&self) -> u64 {
        self.limits.max_open_rows
    }

    /// Decides one request (R-5 to R-12): the deciding rule, or a pending outcome.
    ///
    /// # Errors
    /// [`StoreError::UnknownWorkspace`] for a workspace that is nobody's, or [`StoreError`] if
    /// the pending row can't be written. The caller fails closed.
    pub fn decide(
        &self,
        request: &EgressRequest,
        suffix_allows: SuffixAllows,
    ) -> Result<Decision, StoreError> {
        let now = self.clock.now_ms();
        if let Some(decision) = self.match_rules(request, now, suffix_allows)? {
            return Ok(decision);
        }
        self.decide_locked(&mut lock(&self.conn), request, now, suffix_allows)
    }

    /// The part of [`Store::decide`] that runs with `conn` held.
    fn decide_locked(
        &self,
        conn: &mut Connection,
        request: &EgressRequest,
        now: u64,
        suffix_allows: SuffixAllows,
    ) -> Result<Decision, StoreError> {
        // Snapshots are only swapped under `conn`; look again so a rule committed while we
        // waited for the lock decides instead of a new pending row.
        if let Some(decision) = self.match_rules(request, now, suffix_allows)? {
            return Ok(decision);
        }
        let tx = write_tx(conn)?;
        let head = audit_head(&tx)?;
        let mut fx = Vec::new();
        let outcome = self.record_pending(&tx, request, now, &mut fx)?;
        self.commit(tx, head, fx)?;
        Ok(Decision::Pending(outcome))
    }

    /// What the rules of the workspace's user say about `request`, or `None` when none decides.
    ///
    /// # Errors
    /// [`StoreError::UnknownWorkspace`] for a workspace that is nobody's: it has no rules, and a
    /// pending row for it would reach no one.
    fn match_rules(
        &self,
        request: &EgressRequest,
        now: u64,
        suffix_allows: SuffixAllows,
    ) -> Result<Option<Decision>, StoreError> {
        let index = self.snapshot();
        let user = index
            .owner_of(&request.workspace)
            .ok_or_else(|| StoreError::UnknownWorkspace(request.workspace.clone()))?;
        Ok(index
            .decide_as(user, &request.workspace, &request.host, now, suffix_allows)
            .map(Hit::decision))
    }

    fn record_pending(
        &self,
        tx: &Transaction<'_>,
        request: &EgressRequest,
        now: u64,
        fx: &mut Vec<Event>,
    ) -> Result<PendingOutcome, StoreError> {
        let (workspace, host) = (request.workspace.as_str(), request.host.to_string());
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM pending
                 WHERE workspace_id = ?1 AND host = ?2 AND port = ?3 AND state = 'requested'",
                params![workspace, host, request.port],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let (attempts, last_seen): (i64, i64) = tx.query_row(
                "UPDATE pending SET last_seen = max(last_seen, ?1), attempts = attempts + 1
                 WHERE id = ?2 RETURNING attempts, last_seen",
                params![sql_ts(now), id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            fx.push(Event::PendingUpdated {
                workspace: request.workspace.clone(),
                id,
                attempts: stored_ts("pending", id, attempts)?,
                last_seen: stored_ts("pending", id, last_seen)?,
            });
            return Ok(PendingOutcome::Repeat(PendingId(id)));
        }
        let open: i64 = tx.query_row(
            "SELECT count(*) FROM pending WHERE workspace_id = ?1 AND state = 'requested'",
            [workspace],
            |row| row.get(0),
        )?;
        if u64::try_from(open).unwrap_or(u64::MAX) >= self.limits.max_open_rows {
            tracing::debug!(
                workspace = workspace,
                open,
                "open requests at the cap; this host gets no row until room is made"
            );
            return Ok(PendingOutcome::Unlisted);
        }
        tx.execute(
            "INSERT INTO pending (workspace_id, host, port, first_seen, last_seen)
             VALUES (?1, ?2, ?3, ?4, ?4)",
            params![workspace, host, request.port, sql_ts(now)],
        )?;
        let id = PendingId(tx.last_insert_rowid());
        let row = load_pending(tx, id)?;
        append(
            tx,
            &AuditRecord::PendingCreated {
                ts: now,
                pending: PendingWire::from(&row),
            },
        )?;
        fx.push(Event::PendingOpened {
            request: PendingSummary {
                id: id.0,
                workspace: row.workspace.clone(),
                host: row.host.to_string(),
                registrable_domain: registrable_domain(&row.host),
                port: row.port,
                first_seen: row.first_seen,
                last_seen: row.last_seen,
                attempts: row.attempts,
            },
        });
        Ok(PendingOutcome::New(id))
    }

    /// Commits `tx`, then emits `fx` and, if the transaction appended audit records (the audit's
    /// newest id passed `head`), one [`Event::AuditAppended`] for each user the records belong
    /// to. Called with `conn` held, so events leave in commit order.
    fn commit(&self, tx: Transaction<'_>, head: i64, fx: Vec<Event>) -> Result<(), StoreError> {
        let appended = appended_since(&tx, head)?;
        tx.commit()?;
        self.emit_all(fx, &appended);
        Ok(())
    }

    fn emit_all(&self, fx: Vec<Event>, appended: &[(UserId, i64)]) {
        for event in fx {
            self.events.emit(event);
        }
        for (user, id) in appended {
            self.events.emit(Event::AuditAppended {
                id: *id,
                user: user.get(),
            });
        }
    }

    /// Runs `change` in one transaction, then swaps in the rule set as committed (R-8). The
    /// closure pushes the events its change deserves; they are emitted after the commit.
    fn change<T>(
        &self,
        change: impl FnOnce(&Transaction<'_>, u64, &mut Vec<Event>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut conn = lock(&self.conn);
        let now = self.clock.now_ms();
        let tx = write_tx(&mut conn)?;
        let head = audit_head(&tx)?;
        let mut fx = Vec::new();
        let out = change(&tx, now, &mut fx)?;
        let set = sets::load_index(&tx)?;
        let appended = appended_since(&tx, head)?;
        tx.commit()?;
        *self.rules.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(set);
        self.emit_all(fx, &appended);
        Ok(out)
    }

    /// Creates a rule of `user` and closes the open rows it now decides (R-16): a global rule
    /// becomes theirs, and a workspace or set it names must be theirs. Reached through
    /// [`Store::as_user`].
    ///
    /// # Errors
    /// [`StoreError::SystemActor`], [`StoreError::ExpiryNotInFuture`],
    /// [`StoreError::UnknownWorkspace`] or [`StoreError::UnknownRuleSet`] for another user's
    /// workspace or set, or a database error.
    fn add_rule_by(&self, user: UserId, new: &NewRule) -> Result<Rule, StoreError> {
        if new.created_by == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let rule = self.change(|tx, now, fx| {
            if new.expires_at.is_some_and(|at| at <= now) {
                return Err(StoreError::ExpiryNotInFuture);
            }
            let rule = insert_rule(tx, user, new, now, None)?;
            // A pending row's `decided_by` takes the people's actors and `system`, so a rule
            // made by a workspace create closes the rows it decides as Puddle itself.
            let decider = match new.created_by {
                Actor::WorkspaceCreate => Actor::System,
                actor => actor,
            };
            close_decided_rows(tx, &rule, None, decider, now, fx)?;
            fx.push(Event::RulesChanged {});
            Ok(rule)
        })?;
        tracing::info!(rule = %rule.id, pattern = %rule.pattern, effect = rule.effect.as_str(), "rule created");
        Ok(rule)
    }

    /// What `user`'s rules say about `host` for `workspace` right now, without recording
    /// anything: the answer [`Store::decide`] gives, but `None` where that would open a pending
    /// row. `workspace` may be a name no workspace has yet (a create not made yet): `user`'s
    /// global rules and sets then decide. Reached through [`Store::as_user`].
    fn peek_by(&self, user: UserId, workspace: &WorkspaceId, host: &Host) -> Option<Decision> {
        self.snapshot()
            .decide_as(
                user,
                workspace,
                host,
                self.clock.now_ms(),
                SuffixAllows::Count,
            )
            .map(Hit::decision)
    }

    /// Deletes a rule.
    ///
    /// # Errors
    /// [`StoreError::UnknownRule`] or a database error.
    pub fn delete_rule(&self, id: RuleId, actor: Actor) -> Result<Rule, StoreError> {
        let rule = self.change(|tx, now, fx| {
            let rule = load_rule(tx, id)?;
            tx.execute("DELETE FROM rules WHERE id = ?1", [id.0])?;
            append_for(
                tx,
                &AuditRecord::RuleDeleted {
                    ts: now,
                    rule: RuleWire::from(&rule),
                    reason: RuleDeleteReason::User,
                    actor: actor_str(actor),
                },
                rule.user,
            )?;
            fx.push(Event::RulesChanged {});
            Ok(rule)
        })?;
        tracing::info!(rule = %id, "rule deleted");
        Ok(rule)
    }

    /// Changes a rule's expiry (`None` makes it permanent).
    ///
    /// # Errors
    /// [`StoreError::UnknownRule`], [`StoreError::ExpiryNotInFuture`],
    /// [`StoreError::SystemActor`], or a database error.
    pub fn set_rule_expiry(
        &self,
        id: RuleId,
        expires_at: Option<u64>,
        actor: Actor,
    ) -> Result<Rule, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        self.change(|tx, now, fx| {
            if expires_at.is_some_and(|at| at <= now) {
                return Err(StoreError::ExpiryNotInFuture);
            }
            let before = load_rule(tx, id)?;
            tx.execute(
                "UPDATE rules SET expires_at = ?1 WHERE id = ?2",
                params![expires_at.map(sql_ts), id.0],
            )?;
            let after = load_rule(tx, id)?;
            append_for(
                tx,
                &AuditRecord::RuleUpdated {
                    ts: now,
                    before: RuleWire::from(&before),
                    rule: RuleWire::from(&after),
                    actor: actor_str(actor),
                },
                after.user,
            )?;
            fx.push(Event::RulesChanged {});
            Ok(after)
        })
    }

    /// Approves or denies an open pending row (R-15 to R-17), in one transaction.
    ///
    /// # Errors
    /// [`StoreError::UnknownPending`] or [`StoreError::PendingNotOpen`] for a stale id (nothing
    /// changes), [`StoreError::Pattern`] for a suffix that doesn't cover the host or is a public
    /// suffix, [`StoreError::ExpiryNotInFuture`] for a zero duration,
    /// [`StoreError::SystemActor`], or a database error.
    pub fn resolve_pending(
        &self,
        id: PendingId,
        resolution: &Resolution,
        actor: Actor,
    ) -> Result<Decided, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        if resolution.expires_in.is_some_and(|d| d.is_zero()) {
            return Err(StoreError::ExpiryNotInFuture);
        }
        let decided = self.change(|tx, now, fx| {
            let row = load_pending(tx, id)?;
            if row.state != PendingState::Requested {
                return Err(StoreError::PendingNotOpen {
                    id,
                    state: row.state,
                });
            }
            let pattern = match &resolution.pattern {
                PatternChoice::Exact => Pattern::Exact(row.host.clone()),
                PatternChoice::Suffix(suffix) => Pattern::suffix_covering(&row.host, suffix)?,
            };
            let scope = match resolution.scope {
                ScopeChoice::Workspace => Scope::Workspace(row.workspace.clone()),
                ScopeChoice::Global => Scope::Global,
                ScopeChoice::Set(set) => {
                    sets::require_on(tx, set, &row.workspace)?;
                    Scope::Set(set)
                }
            };
            let new = NewRule {
                scope,
                pattern,
                effect: resolution.effect,
                expires_at: resolution.expires_in.map(|d| plus(now, d)),
                created_by: actor,
            };
            // The rule is the user's whose workspace asked, global or not.
            let user = users::owner_in(tx, &row.workspace)?;
            let rule = insert_rule(tx, user, &new, now, Some(id))?;
            decide_row(tx, &row, &rule, actor, now, fx)?;
            let also_closed = close_decided_rows(tx, &rule, Some(id), actor, now, fx)?;
            fx.push(Event::RulesChanged {});
            Ok(Decided {
                row: load_pending(tx, id)?,
                rule,
                also_closed,
            })
        })?;
        // Bound first: a call inside the macro is expanded twice (the log fallback copy never runs under a subscriber).
        let effect = decided.rule.effect.as_str();
        tracing::info!(
            pending = %id,
            workspace = %decided.row.workspace,
            host = %decided.row.host,
            rule = %decided.rule.id,
            effect,
            "pending request decided"
        );
        Ok(decided)
    }

    /// One pending row.
    ///
    /// # Errors
    /// [`StoreError::UnknownPending`] or a database error.
    pub fn pending(&self, id: PendingId) -> Result<PendingRow, StoreError> {
        load_pending(&lock(&self.conn), id)
    }

    /// Open (`requested`) rows of every user's workspaces, of one workspace or all, most recent
    /// first. For the install's own jobs; what a user is shown comes from [`Store::open_pending`].
    ///
    /// # Errors
    /// A database error, or [`StoreError::Corrupt`].
    pub fn open_pending_every_user(
        &self,
        _: &AllUsers,
        workspace: Option<&WorkspaceId>,
    ) -> Result<Vec<PendingRow>, StoreError> {
        self.open_pending_where(None, workspace)
    }

    /// Open (`requested`) rows of `user`'s workspaces, of one of them or all, most recent first.
    /// A workspace with no recorded user is nobody's, so its rows are in no user's list.
    ///
    /// # Errors
    /// A database error, or [`StoreError::Corrupt`].
    pub fn open_pending(
        &self,
        user: UserId,
        workspace: Option<&WorkspaceId>,
    ) -> Result<Vec<PendingRow>, StoreError> {
        self.open_pending_where(Some(user), workspace)
    }

    fn open_pending_where(
        &self,
        user: Option<UserId>,
        workspace: Option<&WorkspaceId>,
    ) -> Result<Vec<PendingRow>, StoreError> {
        let conn = lock(&self.conn);
        let mut stmt = conn.prepare(&format!(
            "SELECT {PENDING_COLUMNS} FROM pending
             WHERE state = 'requested' AND (?1 IS NULL OR workspace_id = ?1)
               AND (?2 IS NULL OR workspace_id IN
                    (SELECT workspace_id FROM workspace_owners WHERE user_id = ?2))
             ORDER BY last_seen DESC, id DESC"
        ))?;
        let args = params![workspace.map(WorkspaceId::as_str), user.map(UserId::get)];
        let rows = stmt.query_map(args, raw_pending)?;
        rows.map(|raw| pending_from_raw(&raw?)).collect()
    }

    /// [`Store::inbox`] over every user's workspaces, for the install's own jobs.
    ///
    /// # Errors
    /// As [`Store::open_pending`].
    pub fn inbox_every_user(&self, _: &AllUsers) -> Result<Vec<InboxGroup>, StoreError> {
        Ok(group_by_domain(self.open_pending_where(None, None)?))
    }

    /// The open rows of `user`'s workspaces grouped by registrable domain for the inbox (R-18),
    /// most recent group first. Grouping is for display only and decides nothing.
    ///
    /// # Errors
    /// As [`Store::open_pending`].
    pub fn inbox(&self, user: UserId) -> Result<Vec<InboxGroup>, StoreError> {
        Ok(group_by_domain(self.open_pending(user, None)?))
    }
}

/// Open rows, most recent first, grouped by registrable domain in that order.
fn group_by_domain(rows: Vec<PendingRow>) -> Vec<InboxGroup> {
    let mut groups: Vec<InboxGroup> = Vec::new();
    for row in rows {
        let domain = registrable_domain(&row.host);
        match groups.iter_mut().find(|g| g.registrable_domain == domain) {
            Some(group) => group.rows.push(row),
            None => groups.push(InboxGroup {
                registrable_domain: domain,
                rows: vec![row],
            }),
        }
    }
    groups
}

impl Policy for Store {
    fn decide(
        &self,
        request: &EgressRequest,
        suffix_allows: SuffixAllows,
    ) -> Result<Decision, PolicyError> {
        Self::decide(self, request, suffix_allows).map_err(|err| PolicyError {
            reason: err.to_string(),
        })
    }

    fn lookup(
        &self,
        request: &EgressRequest,
        suffix_allows: SuffixAllows,
    ) -> Result<Option<Decision>, PolicyError> {
        self.match_rules(request, self.clock.now_ms(), suffix_allows)
            .map_err(|err| PolicyError {
                reason: err.to_string(),
            })
    }
}

#[cfg(test)]
mod tests;
