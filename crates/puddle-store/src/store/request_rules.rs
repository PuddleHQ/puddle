// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules on the method and path of a request (`docs/spec/rules.md` R-49..R-55). They only ever
//! refuse, so they live apart from the rules on hosts: their own table, their own numbering,
//! and no precedence to resolve.

use puddle_types::{Event, Methods, PathPattern, WorkspaceName};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{Store, append, lock};
use crate::audit::{AuditRecord, RequestRuleWire, RuleDeleteReason, actor_str};
use crate::error::StoreError;
use crate::rule::Actor;

/// Most rules one scope (global, or one workspace) may hold.
pub const MAX_REQUEST_RULES: usize = 500;

/// A rule on method and path, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRule {
    /// Row id, never reused.
    pub id: i64,
    /// The workspace it applies to; `None` for every workspace.
    pub workspace: Option<WorkspaceName>,
    /// The host pattern, `example.com` or `*.example.com`, as the proxy takes it.
    pub host: String,
    /// The methods it names; none for every method.
    pub methods: Methods,
    /// The paths it names.
    pub path: PathPattern,
    /// Epoch ms.
    pub created_at: u64,
    /// Who made it.
    pub created_by: Actor,
}

impl RequestRule {
    fn wire(&self) -> RequestRuleWire {
        RequestRuleWire {
            id: self.id,
            scope: if self.workspace.is_some() {
                "workspace"
            } else {
                "global"
            }
            .to_owned(),
            workspace_id: self.workspace.as_ref().map(ToString::to_string),
            host: self.host.clone(),
            methods: self.methods.names().to_vec(),
            path: self.path.to_string(),
            created_at: self.created_at,
            created_by: actor_str(self.created_by),
        }
    }
}

/// A rule on method and path to create. The host pattern is the caller's to have checked (the
/// proxy's pattern grammar); methods and path are checked types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRequestRule {
    /// The workspace it applies to; `None` for every workspace.
    pub workspace: Option<WorkspaceName>,
    /// The host pattern.
    pub host: String,
    /// The methods; none for every method.
    pub methods: Methods,
    /// The paths.
    pub path: PathPattern,
    /// Who makes it. `system` is refused.
    pub created_by: Actor,
}

const COLUMNS: &str = "id, workspace_id, host, methods, path, created_at, created_by";

fn corrupt(id: i64, reason: String) -> StoreError {
    StoreError::Corrupt {
        table: "request_rules",
        id,
        reason,
    }
}

fn row_to_rule(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<RequestRule, StoreError>> {
    let id: i64 = row.get(0)?;
    let workspace: Option<String> = row.get(1)?;
    let host: String = row.get(2)?;
    let methods: String = row.get(3)?;
    let path: String = row.get(4)?;
    let created_at: i64 = row.get(5)?;
    let created_by: String = row.get(6)?;
    Ok((|| {
        Ok(RequestRule {
            id,
            workspace: workspace
                .map(|w| WorkspaceName::new(&w).map_err(|e| corrupt(id, e.to_string())))
                .transpose()?,
            host,
            methods: Methods::parse(methods.split(',').filter(|m| !m.is_empty()))
                .map_err(|e| corrupt(id, e.to_string()))?,
            path: PathPattern::parse(&path).map_err(|e| corrupt(id, e.to_string()))?,
            created_at: u64::try_from(created_at)
                .map_err(|_| corrupt(id, "negative time".into()))?,
            created_by: Actor::parse(&created_by)
                .ok_or_else(|| corrupt(id, format!("unknown actor {created_by:?}")))?,
        })
    })())
}

fn load(conn: &Connection, id: i64) -> Result<RequestRule, StoreError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM request_rules WHERE id = ?1"),
        [id],
        row_to_rule,
    )
    .optional()?
    .ok_or(StoreError::UnknownRequestRule(id))?
}

fn list(
    conn: &Connection,
    condition: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<RequestRule>, StoreError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM request_rules {condition} ORDER BY id"
    ))?;
    let rows = stmt
        .query_map(args, row_to_rule)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().collect()
}

/// A deleted workspace's rules go with it, each with its own record; the global ones stay.
pub(super) fn delete_workspace_rows(
    tx: &Transaction<'_>,
    workspace: &WorkspaceName,
    now: u64,
    fx: &mut Vec<Event>,
) -> Result<(), StoreError> {
    let rules = list(tx, "WHERE workspace_id = ?1", &[&workspace.as_str()])?;
    tx.execute(
        "DELETE FROM request_rules WHERE workspace_id = ?1",
        [workspace.as_str()],
    )?;
    for rule in &rules {
        append(
            tx,
            &AuditRecord::RequestRuleDeleted {
                ts: now,
                rule: rule.wire(),
                reason: RuleDeleteReason::WorkspaceDeleted,
                actor: actor_str(Actor::System),
            },
        )?;
    }
    if !rules.is_empty() {
        fx.push(Event::RequestRulesChanged {});
    }
    Ok(())
}

impl Store {
    /// Every rule on method and path, or the ones that apply to `workspace` (the global ones and
    /// its own), in id order.
    ///
    /// # Errors
    /// A database error, or a stored row that no longer reads.
    pub fn request_rules(
        &self,
        workspace: Option<&WorkspaceName>,
    ) -> Result<Vec<RequestRule>, StoreError> {
        let conn = lock(&self.conn);
        match workspace {
            None => list(&conn, "", &[]),
            Some(name) => list(
                &conn,
                "WHERE workspace_id IS NULL OR workspace_id = ?1",
                &[&name.as_str()],
            ),
        }
    }

    /// Makes a rule and tells running workspaces to read their rules again.
    ///
    /// # Errors
    /// [`StoreError::SystemActor`], [`StoreError::RequestRuleInvalid`] over the per-scope limit
    /// or an empty host, or a database error.
    pub fn add_request_rule(&self, new: &NewRequestRule) -> Result<RequestRule, StoreError> {
        if new.created_by == Actor::System {
            return Err(StoreError::SystemActor);
        }
        if new.host.trim().is_empty() {
            return Err(StoreError::RequestRuleInvalid("a rule names a host".into()));
        }
        let rule = self.change(|tx, now, fx| {
            let scope = new.workspace.as_ref().map(WorkspaceName::as_str);
            let held: i64 = tx.query_row(
                "SELECT COUNT(*) FROM request_rules WHERE workspace_id IS ?1",
                [scope],
                |r| r.get(0),
            )?;
            if usize::try_from(held).unwrap_or(usize::MAX) >= MAX_REQUEST_RULES {
                return Err(StoreError::RequestRuleInvalid(format!(
                    "at most {MAX_REQUEST_RULES} rules on method and path in one scope"
                )));
            }
            let now_i = i64::try_from(now).unwrap_or(i64::MAX);
            tx.execute(
                "INSERT INTO request_rules
                     (scope, workspace_id, host, methods, path, created_at, created_by)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    if scope.is_some() {
                        "workspace"
                    } else {
                        "global"
                    },
                    scope,
                    new.host,
                    new.methods.to_string(),
                    new.path.to_string(),
                    now_i,
                    new.created_by.as_str(),
                ],
            )?;
            let rule = load(tx, tx.last_insert_rowid())?;
            append(
                tx,
                &AuditRecord::RequestRuleCreated {
                    ts: now,
                    rule: rule.wire(),
                },
            )?;
            fx.push(Event::RequestRulesChanged {});
            Ok(rule)
        })?;
        tracing::info!(rule = rule.id, host = %rule.host, "request rule created");
        Ok(rule)
    }

    /// Deletes a rule and tells running workspaces to read their rules again.
    ///
    /// # Errors
    /// [`StoreError::UnknownRequestRule`], [`StoreError::SystemActor`] or a database error.
    pub fn delete_request_rule(&self, id: i64, actor: Actor) -> Result<RequestRule, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let rule = self.change(|tx, now, fx| {
            let rule = load(tx, id)?;
            tx.execute("DELETE FROM request_rules WHERE id = ?1", [id])?;
            append(
                tx,
                &AuditRecord::RequestRuleDeleted {
                    ts: now,
                    rule: rule.wire(),
                    reason: RuleDeleteReason::User,
                    actor: actor_str(actor),
                },
            )?;
            fx.push(Event::RequestRulesChanged {});
            Ok(rule)
        })?;
        tracing::info!(rule = id, "request rule deleted");
        Ok(rule)
    }
}
