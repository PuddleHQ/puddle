// SPDX-License-Identifier: GPL-3.0-or-later
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
    ConnectionEvent, ConnectionLog, ConnectionOrigin, Decision, EgressRequest, Event, EventSink,
    Host, NullSink, PendingEnd, PendingId, PendingOutcome, PendingSummary, Policy, PolicyError,
    RuleId, SuffixAllows, WorkspaceId,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::audit::{
    AuditOutcome, AuditRecord, ConnectionRecord, ConnectionWindow, PendingExpiryReason,
    PendingWire, RuleDeleteReason, RuleWire, WorkspaceOpKind, WorkspaceOpOutcome, actor_str,
};
use crate::clock::Clock;
use crate::engine::{Hit, RuleIndex};
use crate::error::StoreError;
use crate::pattern::{Pattern, SuffixPattern, registrable_domain};
use crate::pending::{
    Decided, InboxGroup, PatternChoice, PendingRow, PendingState, Resolution, ScopeChoice,
};
use crate::rule::{Actor, Effect, NewRule, Rule, Scope};
use crate::schema;
use crate::users::UserId;

mod devices;
mod environment;
#[cfg(test)]
mod failures;
mod identities;
mod interception;
mod learned;
mod ports;
mod request_rules;
mod sets;
mod shell_init;
mod ssh_agent;
pub(crate) mod users;

pub use interception::{
    DEFAULT_LEARNED_CAP, DEFAULT_LEARNED_TTL_SECS, InterceptionSettings, LearnedLimits,
    WorkspaceInterception,
};
pub use learned::LearnedRow;
pub use ports::PinnedPort;
pub use request_rules::{MAX_REQUEST_RULES, NewRequestRule, RequestRule};
pub use sets::{RuleSetEntryInfo, RuleSetInfo, SystemHost, SystemPlan, parse_rule_set};
pub use ssh_agent::{MAX_SSH_AGENT_HOSTS, MAX_SSH_AGENT_KEYS, SshAgentHost};
pub use users::WorkspaceOwners;

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
    pub fn rules_every_user(&self) -> Vec<Rule> {
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
    /// [`StoreError`] if the pending row can't be written. The caller fails closed.
    pub fn decide(
        &self,
        request: &EgressRequest,
        suffix_allows: SuffixAllows,
    ) -> Result<Decision, StoreError> {
        let now = self.clock.now_ms();
        if let Some(decision) = self.match_rules(request, now, suffix_allows) {
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
        if let Some(decision) = self.match_rules(request, now, suffix_allows) {
            return Ok(decision);
        }
        let tx = write_tx(conn)?;
        let head = audit_head(&tx)?;
        let mut fx = Vec::new();
        let outcome = self.record_pending(&tx, request, now, &mut fx)?;
        self.commit(tx, head, fx)?;
        Ok(Decision::Pending(outcome))
    }

    fn match_rules(
        &self,
        request: &EgressRequest,
        now: u64,
        suffix_allows: SuffixAllows,
    ) -> Option<Decision> {
        let index = self.snapshot();
        index
            .decide(&request.workspace, &request.host, now, suffix_allows)
            .map(Hit::decision)
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

    /// Creates a rule and closes the open rows it now decides (R-16). A workspace rule is the
    /// workspace's user's, a set's entry the set's user's, and a global rule the owner's: see
    /// [`Store::add_rule_as`] for a rule another user makes.
    ///
    /// # Errors
    /// [`StoreError::SystemActor`], [`StoreError::ExpiryNotInFuture`], or a database error.
    pub fn add_rule(&self, new: &NewRule) -> Result<Rule, StoreError> {
        self.add_rule_by(None, new)
    }

    /// [`Store::add_rule`] by `user`: a global rule becomes theirs, and a workspace or set it
    /// names must be theirs.
    ///
    /// # Errors
    /// As [`Store::add_rule`], and [`StoreError::UnknownWorkspace`] or
    /// [`StoreError::UnknownRuleSet`] for another user's workspace or set.
    pub fn add_rule_as(&self, user: UserId, new: &NewRule) -> Result<Rule, StoreError> {
        self.add_rule_by(Some(user), new)
    }

    fn add_rule_by(&self, user: Option<UserId>, new: &NewRule) -> Result<Rule, StoreError> {
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

    /// What the rules say about `host` for `workspace` right now, without recording anything:
    /// the same answer [`Store::decide`] gives, but `None` where that would open a pending row.
    /// A workspace that does not exist yet sees only the global rules and the sets' defaults.
    #[must_use]
    pub fn peek(&self, workspace: &WorkspaceId, host: &Host) -> Option<Decision> {
        self.match_rules(
            &EgressRequest::new(workspace.clone(), host.clone(), 443),
            self.clock.now_ms(),
            SuffixAllows::Count,
        )
    }

    /// [`Store::peek`] for a workspace of `user` that is not made yet (`workspace` is a name no
    /// workspace has): `user`'s global rules and sets.
    #[must_use]
    pub fn peek_as(&self, user: UserId, workspace: &WorkspaceId, host: &Host) -> Option<Decision> {
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

    /// Allows each of `hosts` for `workspace` that no rule decides yet, with an exact allow rule
    /// made by [`Actor::WorkspaceCreate`] (a create names the hosts of its repositories, and the
    /// clone that follows would otherwise wait in the inbox). A host that is allowed already, or
    /// that a deny decides, gets no rule: a create never overrides a block. Returns the rules
    /// made.
    ///
    /// # Errors
    /// A database error; the rules made before it stay.
    pub fn allow_hosts_for_create(
        &self,
        workspace: &WorkspaceId,
        hosts: &[Host],
    ) -> Result<Vec<Rule>, StoreError> {
        let mut made = Vec::new();
        for host in hosts {
            if self.peek(workspace, host).is_some() {
                continue;
            }
            made.push(self.add_rule(&NewRule {
                scope: Scope::Workspace(workspace.clone()),
                pattern: Pattern::Exact(host.clone()),
                effect: Effect::Allow,
                expires_at: None,
                created_by: Actor::WorkspaceCreate,
            })?);
        }
        Ok(made)
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
            let rule = insert_rule(tx, Some(user), &new, now, Some(id))?;
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
    pub fn inbox_every_user(&self) -> Result<Vec<InboxGroup>, StoreError> {
        Ok(group_by_domain(self.open_pending_every_user(None)?))
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
            if identities::delete_workspace_rows(tx, workspace)? {
                fx.push(Event::WorkspaceGitChanged {
                    workspace: workspace.clone(),
                });
            }
            // Its variables and stand-ins go with it; the global ones stay.
            let (secrets, had_env) = environment::delete_workspace_rows(tx, workspace)?;
            if had_env {
                fx.push(Event::WorkspaceEnvChanged {
                    workspace: workspace.clone(),
                });
            }
            shell_init::delete_workspace_rows(tx, workspace)?;
            // Its pinned forwards go with it.
            ports::delete_workspace_rows(tx, workspace)?;
            interception::delete_workspace_rows(tx, workspace)?;
            learned::delete_workspace_rows(tx, workspace)?;
            request_rules::delete_workspace_rows(tx, workspace, now, fx)?;
            ssh_agent::delete_workspace_rows(tx, workspace)?;
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
        self.audit_query_every_user(&filter, cursor, limit)
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
fn audit_query_sql(
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
        Ok(self.match_rules(request, self.clock.now_ms(), suffix_allows))
    }
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

fn audit_bytes(conn: &Connection) -> Result<u64, StoreError> {
    let bytes: i64 = conn.query_row("SELECT bytes FROM audit_size WHERE id = 1", [], |row| {
        row.get(0)
    })?;
    // A negative size read as 0 would keep the audit-size cap from ever firing.
    u64::try_from(bytes).map_err(|_| corrupt("audit_size", 1, "negative byte count"))
}

/// Appends a record of the workspace's user it names; one that names none is the owner's (the
/// install's own: Puddle's connections, a trim, the built-in sets and System managed).
fn append(conn: &Connection, record: &AuditRecord) -> Result<(), StoreError> {
    append_by(conn, record, None)
}

/// Appends a record of `user`: one whose workspace (if it names one) is `user`'s.
fn append_for(conn: &Connection, record: &AuditRecord, user: UserId) -> Result<(), StoreError> {
    append_by(conn, record, Some(user))
}

fn append_by(
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
fn appended_since(conn: &Connection, head: i64) -> Result<Vec<(UserId, i64)>, StoreError> {
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
fn audit_head(conn: &Connection) -> Result<i64, StoreError> {
    Ok(
        conn.query_row("SELECT coalesce(max(id), 0) FROM audit", [], |row| {
            row.get(0)
        })?,
    )
}

/// Writes a rule of `user` (see [`Store::add_rule_as`]; `None` takes the scope's user, and the
/// owner for a global rule).
fn insert_rule(
    tx: &Transaction<'_>,
    user: Option<UserId>,
    new: &NewRule,
    now: u64,
    source: Option<PendingId>,
) -> Result<Rule, StoreError> {
    let user = match (&new.scope, user) {
        (Scope::Workspace(workspace), Some(user)) => {
            users::require_owner(tx, workspace, user)?;
            user
        }
        (Scope::Workspace(workspace), None) => users::owner_in(tx, workspace)?,
        (Scope::Set(set), Some(user)) => {
            sets::require_user_set(tx, user, *set)?;
            user
        }
        (Scope::Set(set), None) => sets::set_user(tx, *set)?,
        (Scope::Global, user) => user.unwrap_or(UserId::OWNER),
    };
    let workspace = new.scope.workspace().map(WorkspaceId::as_str);
    let kind = match new.pattern {
        Pattern::Exact(_) => "exact",
        Pattern::Suffix(_) => "suffix",
    };
    tx.execute(
        "INSERT INTO rules (scope, workspace_id, set_id, pattern_kind, pattern, effect, expires_at,
                            created_at, created_by, source_pending_id, user_id)
         VALUES (?1, ?2, ?10, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?11)",
        params![
            new.scope.as_str(),
            workspace,
            kind,
            new.pattern.to_string(),
            new.effect.as_str(),
            new.expires_at.map(sql_ts),
            sql_ts(now),
            new.created_by.as_str(),
            source.map(|id| id.0),
            new.scope.set(),
            user.get(),
        ],
    )?;
    let rule = load_rule(tx, RuleId(tx.last_insert_rowid()))?;
    append_for(
        tx,
        &AuditRecord::RuleCreated {
            ts: now,
            rule: RuleWire::from(&rule),
        },
        rule.user,
    )?;
    Ok(rule)
}

/// Sets an open row to the state `rule` gives it and records that.
fn decide_row(
    tx: &Transaction<'_>,
    row: &PendingRow,
    rule: &Rule,
    actor: Actor,
    now: u64,
    fx: &mut Vec<Event>,
) -> Result<(), StoreError> {
    decide_row_by(tx, row, Hit::Rule(rule), actor, now, fx)
}

/// Sets an open row to the state `hit` gives it and records that.
fn decide_row_by(
    tx: &Transaction<'_>,
    row: &PendingRow,
    hit: Hit<'_>,
    actor: Actor,
    now: u64,
    fx: &mut Vec<Event>,
) -> Result<(), StoreError> {
    let effect = hit.effect();
    tx.execute(
        "UPDATE pending SET state = ?1, decided_at = ?2, decided_by = ?3, rule_id = ?4,
                            rule_set = ?6
         WHERE id = ?5 AND state = 'requested'",
        params![
            PendingState::decided_by(effect).as_str(),
            sql_ts(now),
            actor.as_str(),
            hit.rule_id().map(|id| id.0),
            row.id.0,
            hit.rule_set().map(|set| set.to_string()),
        ],
    )?;
    append(
        tx,
        &AuditRecord::PendingDecided {
            ts: now,
            pending: PendingWire::from(&load_pending(tx, row.id)?),
        },
    )?;
    fx.push(Event::PendingClosed {
        workspace: row.workspace.clone(),
        id: row.id.0,
        state: match effect {
            Effect::Allow => PendingEnd::Allowed,
            Effect::Deny => PendingEnd::Denied,
        },
        rule_id: hit.rule_id().map(|id| id.0),
    });
    Ok(())
}

/// Closes every other open row that `rule` now decides, the same way (R-16): rows in the rule's
/// workspace (any workspace for a global rule) for which `rule` is the winning rule.
fn close_decided_rows(
    tx: &Transaction<'_>,
    rule: &Rule,
    except: Option<PendingId>,
    actor: Actor,
    now: u64,
    fx: &mut Vec<Event>,
) -> Result<Vec<PendingId>, StoreError> {
    let set = sets::load_index(tx)?;
    let mut stmt = tx.prepare(&format!(
        "SELECT {PENDING_COLUMNS} FROM pending
         WHERE state = 'requested' AND (?1 IS NULL OR workspace_id = ?1) ORDER BY id"
    ))?;
    let workspace = rule.scope.workspace().map(WorkspaceId::as_str);
    let open = stmt
        .query_map([workspace], raw_pending)?
        .map(|raw| pending_from_raw(&raw?))
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    let mut closed = Vec::new();
    for row in open {
        if Some(row.id) == except || !rule.pattern.matches(&row.host) {
            continue;
        }
        let winner = set.decide(&row.workspace, &row.host, now, SuffixAllows::Count);
        if winner.is_some_and(|w| w.rule_id() == Some(rule.id)) {
            decide_row(tx, &row, rule, actor, now, fx)?;
            closed.push(row.id);
        }
    }
    Ok(closed)
}

/// Expires open rows matching `filter` (which binds `?2` to `value`) and records each.
fn expire_rows(
    tx: &Transaction<'_>,
    filter: &str,
    value: &dyn rusqlite::ToSql,
    now: u64,
    reason: PendingExpiryReason,
    fx: &mut Vec<Event>,
) -> Result<u64, StoreError> {
    let mut stmt = tx.prepare(&format!(
        "UPDATE pending SET state = 'expired', decided_at = ?1, decided_by = 'system'
         WHERE state = 'requested' AND {filter}
         RETURNING {PENDING_COLUMNS}"
    ))?;
    let rows = stmt
        .query_map(params![sql_ts(now), value], raw_pending)?
        .map(|raw| pending_from_raw(&raw?))
        .collect::<Result<Vec<_>, _>>()?;
    drop(stmt);
    for row in &rows {
        append(
            tx,
            &AuditRecord::PendingExpired {
                ts: now,
                pending: PendingWire::from(row),
                reason,
            },
        )?;
        fx.push(Event::PendingClosed {
            workspace: row.workspace.clone(),
            id: row.id.0,
            state: PendingEnd::Expired,
            rule_id: None,
        });
    }
    Ok(rows.len() as u64)
}

const RULE_COLUMNS: &str = "id, scope, workspace_id, pattern_kind, pattern, effect, expires_at, \
                            created_at, created_by, source_pending_id, set_id, user_id";

/// A `rules` row as SQLite holds it, before validation.
struct RawRule {
    id: i64,
    scope: String,
    workspace_id: Option<String>,
    pattern_kind: String,
    pattern: String,
    effect: String,
    expires_at: Option<i64>,
    created_at: i64,
    created_by: String,
    source_pending_id: Option<i64>,
    set_id: Option<i64>,
    user_id: i64,
}

fn raw_rule(row: &Row<'_>) -> rusqlite::Result<RawRule> {
    Ok(RawRule {
        id: row.get(0)?,
        scope: row.get(1)?,
        workspace_id: row.get(2)?,
        pattern_kind: row.get(3)?,
        pattern: row.get(4)?,
        effect: row.get(5)?,
        expires_at: row.get(6)?,
        created_at: row.get(7)?,
        created_by: row.get(8)?,
        source_pending_id: row.get(9)?,
        set_id: row.get(10)?,
        user_id: row.get(11)?,
    })
}

fn corrupt(table: &'static str, id: i64, reason: impl Into<String>) -> StoreError {
    StoreError::Corrupt {
        table,
        id,
        reason: reason.into(),
    }
}

fn stored_ts(table: &'static str, id: i64, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| corrupt(table, id, "negative timestamp"))
}

fn rule_from_raw(raw: &RawRule) -> Result<Rule, StoreError> {
    let bad = |reason: &str| corrupt("rules", raw.id, reason);
    let scope = match (raw.scope.as_str(), raw.workspace_id.as_deref(), raw.set_id) {
        ("global", None, None) => Scope::Global,
        ("workspace", Some(id), None) => {
            Scope::Workspace(WorkspaceId::new(id).map_err(|e| bad(&e.to_string()))?)
        }
        ("set", None, Some(set)) => Scope::Set(set),
        _ => return Err(bad("scope")),
    };
    let host = |text: &str| Host::parse_normalised(text).map_err(|e| bad(&e.to_string()));
    // Stored suffixes are not re-checked against the public suffix list: a list update must not
    // make an existing database unreadable.
    let pattern = match (raw.pattern_kind.as_str(), raw.pattern.strip_prefix('.')) {
        ("exact", None) => Pattern::Exact(host(&raw.pattern)?),
        ("suffix", Some(base)) => match host(base)? {
            Host::Name(name) => Pattern::Suffix(SuffixPattern::from_stored(name)),
            Host::Ip(_) => return Err(bad("suffix of an ip literal")),
        },
        _ => return Err(bad("pattern")),
    };
    let effect = match raw.effect.as_str() {
        "allow" => Effect::Allow,
        "deny" => Effect::Deny,
        _ => return Err(bad("effect")),
    };
    Ok(Rule {
        id: RuleId(raw.id),
        user: UserId::from_stored(raw.user_id),
        scope,
        pattern,
        effect,
        expires_at: raw
            .expires_at
            .map(|t| stored_ts("rules", raw.id, t))
            .transpose()?,
        created_at: stored_ts("rules", raw.id, raw.created_at)?,
        created_by: Actor::parse(&raw.created_by).ok_or_else(|| bad("created_by"))?,
        source_pending_id: raw.source_pending_id.map(PendingId),
    })
}

fn load_rules(conn: &Connection) -> Result<Vec<Rule>, StoreError> {
    let mut stmt = conn.prepare(&format!("SELECT {RULE_COLUMNS} FROM rules ORDER BY id"))?;
    let rules = stmt.query_map([], raw_rule)?;
    rules.map(|raw| rule_from_raw(&raw?)).collect()
}

fn load_rule(conn: &Connection, id: RuleId) -> Result<Rule, StoreError> {
    let raw = conn
        .query_row(
            &format!("SELECT {RULE_COLUMNS} FROM rules WHERE id = ?1"),
            [id.0],
            raw_rule,
        )
        .optional()?
        .ok_or(StoreError::UnknownRule(id))?;
    rule_from_raw(&raw)
}

const PENDING_COLUMNS: &str = "id, workspace_id, host, port, first_seen, last_seen, attempts, \
                               state, decided_at, decided_by, rule_id, rule_set";

/// A `pending` row as SQLite holds it, before validation.
struct RawPending {
    id: i64,
    workspace_id: String,
    host: String,
    port: i64,
    first_seen: i64,
    last_seen: i64,
    attempts: i64,
    state: String,
    decided_at: Option<i64>,
    decided_by: Option<String>,
    rule_id: Option<i64>,
    rule_set: Option<String>,
}

fn raw_pending(row: &Row<'_>) -> rusqlite::Result<RawPending> {
    Ok(RawPending {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        host: row.get(2)?,
        port: row.get(3)?,
        first_seen: row.get(4)?,
        last_seen: row.get(5)?,
        attempts: row.get(6)?,
        state: row.get(7)?,
        decided_at: row.get(8)?,
        decided_by: row.get(9)?,
        rule_id: row.get(10)?,
        rule_set: row.get(11)?,
    })
}

fn pending_from_raw(raw: &RawPending) -> Result<PendingRow, StoreError> {
    let bad = |reason: &str| corrupt("pending", raw.id, reason);
    let ts = |value: i64| stored_ts("pending", raw.id, value);
    Ok(PendingRow {
        id: PendingId(raw.id),
        workspace: WorkspaceId::new(&raw.workspace_id).map_err(|e| bad(&e.to_string()))?,
        host: Host::parse_normalised(&raw.host).map_err(|e| bad(&e.to_string()))?,
        port: u16::try_from(raw.port).map_err(|_| bad("port"))?,
        first_seen: ts(raw.first_seen)?,
        last_seen: ts(raw.last_seen)?,
        attempts: u64::try_from(raw.attempts).map_err(|_| bad("attempts"))?,
        state: PendingState::parse(&raw.state).ok_or_else(|| bad("state"))?,
        decided_at: raw.decided_at.map(ts).transpose()?,
        decided_by: raw
            .decided_by
            .as_deref()
            .map(|a| Actor::parse(a).ok_or_else(|| bad("decided_by")))
            .transpose()?,
        rule_id: raw.rule_id.map(RuleId),
        rule_set: raw.rule_set.clone(),
    })
}

fn load_pending(conn: &Connection, id: PendingId) -> Result<PendingRow, StoreError> {
    let raw = conn
        .query_row(
            &format!("SELECT {PENDING_COLUMNS} FROM pending WHERE id = ?1"),
            [id.0],
            raw_pending,
        )
        .optional()?
        .ok_or(StoreError::UnknownPending(id))?;
    pending_from_raw(&raw)
}

#[cfg(test)]
mod tests;
