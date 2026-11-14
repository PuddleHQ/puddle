// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rule and pending rows: their SQL and the decoding of what the database stores.

use puddle_types::{Event, Host, PendingEnd, PendingId, RuleId, SuffixAllows, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use crate::audit::{AuditRecord, PendingExpiryReason, PendingWire, RuleWire};
use crate::engine::Hit;
use crate::error::StoreError;
use crate::pattern::{Pattern, SuffixPattern};
use crate::pending::{PendingRow, PendingState};
use crate::rule::{Actor, Effect, NewRule, Rule, Scope};
use crate::users::UserId;

use super::audit::{append, append_for};
use super::sql_ts;

/// Writes a rule of `user`; a workspace or set scope must be theirs.
pub(super) fn insert_rule(
    tx: &Transaction<'_>,
    user: UserId,
    new: &NewRule,
    now: u64,
    source: Option<PendingId>,
) -> Result<Rule, StoreError> {
    match &new.scope {
        Scope::Workspace(workspace) => super::users::require_owner(tx, workspace, user)?,
        Scope::Set(set) => {
            super::sets::require_user_set(tx, user, *set)?;
        }
        Scope::Global => {}
    }
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
pub(super) fn decide_row(
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
pub(super) fn decide_row_by(
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
pub(super) fn close_decided_rows(
    tx: &Transaction<'_>,
    rule: &Rule,
    except: Option<PendingId>,
    actor: Actor,
    now: u64,
    fx: &mut Vec<Event>,
) -> Result<Vec<PendingId>, StoreError> {
    let set = super::sets::load_index(tx)?;
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
pub(super) fn expire_rows(
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

pub(super) const RULE_COLUMNS: &str = "id, scope, workspace_id, pattern_kind, pattern, effect, expires_at, \
                            created_at, created_by, source_pending_id, set_id, user_id";

/// A `rules` row as SQLite holds it, before validation.
pub(super) struct RawRule {
    pub(super) id: i64,
    pub(super) scope: String,
    pub(super) workspace_id: Option<String>,
    pub(super) pattern_kind: String,
    pub(super) pattern: String,
    pub(super) effect: String,
    pub(super) expires_at: Option<i64>,
    pub(super) created_at: i64,
    pub(super) created_by: String,
    pub(super) source_pending_id: Option<i64>,
    pub(super) set_id: Option<i64>,
    pub(super) user_id: i64,
}

pub(super) fn raw_rule(row: &Row<'_>) -> rusqlite::Result<RawRule> {
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

pub(super) fn corrupt(table: &'static str, id: i64, reason: impl Into<String>) -> StoreError {
    StoreError::Corrupt {
        table,
        id,
        reason: reason.into(),
    }
}

pub(super) fn stored_ts(table: &'static str, id: i64, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| corrupt(table, id, "negative timestamp"))
}

pub(super) fn rule_from_raw(raw: &RawRule) -> Result<Rule, StoreError> {
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

pub(super) fn load_rules(conn: &Connection) -> Result<Vec<Rule>, StoreError> {
    let mut stmt = conn.prepare(&format!("SELECT {RULE_COLUMNS} FROM rules ORDER BY id"))?;
    let rules = stmt.query_map([], raw_rule)?;
    rules.map(|raw| rule_from_raw(&raw?)).collect()
}

pub(super) fn load_rule(conn: &Connection, id: RuleId) -> Result<Rule, StoreError> {
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

pub(super) const PENDING_COLUMNS: &str = "id, workspace_id, host, port, first_seen, last_seen, attempts, \
                               state, decided_at, decided_by, rule_id, rule_set";

/// A `pending` row as SQLite holds it, before validation.
pub(super) struct RawPending {
    pub(super) id: i64,
    pub(super) workspace_id: String,
    pub(super) host: String,
    pub(super) port: i64,
    pub(super) first_seen: i64,
    pub(super) last_seen: i64,
    pub(super) attempts: i64,
    pub(super) state: String,
    pub(super) decided_at: Option<i64>,
    pub(super) decided_by: Option<String>,
    pub(super) rule_id: Option<i64>,
    pub(super) rule_set: Option<String>,
}

pub(super) fn raw_pending(row: &Row<'_>) -> rusqlite::Result<RawPending> {
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

pub(super) fn pending_from_raw(raw: &RawPending) -> Result<PendingRow, StoreError> {
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

pub(super) fn load_pending(conn: &Connection, id: PendingId) -> Result<PendingRow, StoreError> {
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
