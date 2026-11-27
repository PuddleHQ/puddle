// SPDX-License-Identifier: AGPL-3.0-or-later
//! Switching a set, or one built-in entry, on or off.

use puddle_types::{Event, PendingId, RuleSetId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::index::{load_hidden, load_index, parse_rule_set};
use super::user_sets::require_user_set;
use crate::audit::{AuditRecord, RuleSwitchKind, actor_str};
use crate::catalogue::{self};
use crate::engine::RuleIndex;
use crate::error::StoreError;
use crate::pattern::Pattern;
use crate::rule::Actor;
use crate::store::rows::open_rows;
use crate::store::{Store, append_for, decide_row_by, sql_ts, users};
use crate::users::UserId;

/// Fails unless the set is one the workspace's user made and is on for `workspace`: approving
/// into a set that is off there would not allow the request (R-38).
pub(in crate::store) fn require_on(
    conn: &Connection,
    id: i64,
    workspace: &WorkspaceId,
) -> Result<(), StoreError> {
    let index = load_index(conn)?;
    let user = index
        .owner_of(workspace)
        .ok_or_else(|| StoreError::UnknownWorkspace(workspace.clone()))?;
    require_user_set(conn, user, id)?;
    if index.is_on(RuleSetId::User(id), workspace) {
        Ok(())
    } else {
        Err(StoreError::RuleSetOff {
            set: RuleSetId::User(id).to_string(),
            workspace: workspace.to_string(),
        })
    }
}

/// Closes the open rows (in `workspace`, or in any workspace of `user`, or of anyone when `None`)
/// that `index` now decides, the way it decides them (R-37: like a new rule, R-16). Returns the
/// rows it closed.
pub(super) fn close_rows_now_decided(
    tx: &Transaction<'_>,
    index: &RuleIndex,
    user: Option<UserId>,
    workspace: Option<&WorkspaceId>,
    actor: Actor,
    now: u64,
    fx: &mut Vec<Event>,
) -> Result<Vec<PendingId>, StoreError> {
    let mut closed = Vec::new();
    for (row, suffix_allows) in &open_rows(tx, workspace)? {
        if user.is_some_and(|user| index.owner_of(&row.workspace) != Some(user)) {
            continue;
        }
        let Some(hit) = index.decide(&row.workspace, &row.host, now, *suffix_allows) else {
            continue;
        };
        decide_row_by(tx, row, hit, actor, now, fx)?;
        closed.push(row.id);
    }
    Ok(closed)
}

impl Store {
    /// Switches a set on or off for every workspace of `user` (`workspace` `None`) or for one of
    /// theirs, or back to following the next level (`enabled` `None`), and closes the open
    /// requests the set now decides (R-37). Returns the rows it closed.
    ///
    /// # Errors
    /// [`StoreError::NotSwitchable`] for System managed, [`StoreError::UnknownRuleSet`] (also a
    /// set another user made), [`StoreError::UnknownWorkspace`] for another user's workspace,
    /// [`StoreError::SystemActor`], or a database error.
    pub fn switch_rule_set(
        &self,
        user: UserId,
        set: RuleSetId,
        workspace: Option<&WorkspaceId>,
        enabled: Option<bool>,
        actor: Actor,
    ) -> Result<Vec<PendingId>, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let closed = self.change(|tx, now, fx| {
            if set == RuleSetId::System {
                return Err(StoreError::NotSwitchable);
            }
            if let RuleSetId::User(id) = set {
                require_user_set(tx, user, id)?;
            }
            if let Some(workspace) = workspace {
                users::require_owner(tx, workspace, user)?;
            }
            // Built-in slugs this Puddle doesn't ship, and sets of a kind added later, don't parse.
            if parse_rule_set(&set.to_string()) != Some(set) {
                return Err(StoreError::UnknownRuleSet(set.to_string()));
            }
            if let RuleSetId::BuiltIn(slug) = set
                && load_hidden(tx)?.contains(slug)
            {
                return Err(StoreError::RuleSetHidden(set.to_string()));
            }
            let key = set.to_string();
            let name = workspace.map(WorkspaceId::as_str);
            tx.execute(
                "DELETE FROM rule_set_switches
                 WHERE rule_set = ?1 AND workspace_id IS ?2 AND user_id = ?3",
                params![key, name, user.get()],
            )?;
            if let Some(on) = enabled {
                tx.execute(
                    "INSERT INTO rule_set_switches
                         (rule_set, workspace_id, enabled, changed_at, user_id)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![key, name, i64::from(on), sql_ts(now), user.get()],
                )?;
            }
            let record = AuditRecord::RuleSetSwitched {
                ts: now,
                set_id: key,
                workspace_id: name.map(str::to_owned),
                enabled,
                actor: actor_str(actor),
            };
            append_for(tx, &record, user)?;
            let index = load_index(tx)?;
            let closed = close_rows_now_decided(tx, &index, Some(user), workspace, actor, now, fx)?;
            fx.push(Event::RulesChanged {});
            Ok(closed)
        })?;
        tracing::info!(%set, workspace = ?workspace.map(WorkspaceId::as_str), ?enabled, closed = closed.len(), "rule set switched");
        Ok(closed)
    }

    /// Switches one entry of a built-in set on or off for every workspace of `user` (R-65), and
    /// closes the open requests that decides. The entry is named as the catalogue writes it
    /// (`github.com`, `*.github.com`). Writes one `rule_switched` record, and none when the entry
    /// is already in that state. Returns the rows it closed.
    ///
    /// # Errors
    /// [`StoreError::NotSwitchable`] for System managed, [`StoreError::UnknownRuleSet`] for a set
    /// that is not built in or has no such entry, [`StoreError::RuleSetHidden`],
    /// [`StoreError::SystemActor`], or a database error.
    pub fn switch_builtin_entry(
        &self,
        user: UserId,
        set: RuleSetId,
        pattern: &str,
        enabled: bool,
        actor: Actor,
    ) -> Result<Vec<PendingId>, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let closed = self.change(|tx, now, fx| {
            let unknown = || StoreError::UnknownRuleSet(format!("{set} entry {pattern}"));
            let slug = match set {
                RuleSetId::System => return Err(StoreError::NotSwitchable),
                RuleSetId::BuiltIn(slug) => slug,
                _ => return Err(unknown()),
            };
            let built_in = catalogue::built_in(slug).ok_or_else(unknown)?;
            if load_hidden(tx)?.contains(slug) {
                return Err(StoreError::RuleSetHidden(set.to_string()));
            }
            let pattern = Pattern::parse(pattern)?.to_string();
            if !catalogue::patterns(built_in.entries)
                .iter()
                .any(|p| p.to_string() == pattern)
            {
                return Err(unknown());
            }
            let key = set.to_string();
            let off_before = tx
                .query_row(
                    "SELECT 1 FROM builtin_entry_switches
                     WHERE user_id = ?1 AND rule_set = ?2 AND pattern = ?3",
                    params![user.get(), key, pattern],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if off_before != enabled {
                return Ok(Vec::new());
            }
            if enabled {
                tx.execute(
                    "DELETE FROM builtin_entry_switches
                     WHERE user_id = ?1 AND rule_set = ?2 AND pattern = ?3",
                    params![user.get(), key, pattern],
                )?;
            } else {
                tx.execute(
                    "INSERT INTO builtin_entry_switches (user_id, rule_set, pattern, changed_at)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![user.get(), key, pattern, sql_ts(now)],
                )?;
            }
            append_for(
                tx,
                &AuditRecord::RuleSwitched {
                    ts: now,
                    kind: RuleSwitchKind::BuiltinEntry,
                    id: None,
                    set_id: Some(key),
                    pattern,
                    workspace_id: None,
                    enabled,
                    actor: actor_str(actor),
                },
                user,
            )?;
            let index = load_index(tx)?;
            let closed = close_rows_now_decided(tx, &index, Some(user), None, actor, now, fx)?;
            fx.push(Event::RulesChanged {});
            Ok(closed)
        })?;
        tracing::info!(%set, ?enabled, closed = closed.len(), "built-in entry switched");
        Ok(closed)
    }
}
