// SPDX-License-Identifier: AGPL-3.0-or-later
//! Changing rules: add, edit, switch, expire, delete and undo a decision.

use puddle_types::{Decision, Event, Host, RuleId, SuffixAllows, WorkspaceId};
use rusqlite::params;

use crate::audit::{AuditRecord, RuleSwitchKind, RuleWire, actor_str};
use crate::engine::Hit;
use crate::error::StoreError;
use crate::pattern::Pattern;
use crate::pending::PendingRow;
use crate::rule::{Actor, NewRule, Precedence, Rule, RuleEdit, Scope};
use crate::users::UserId;

use super::rows::{
    close_decided_rows, closed_by_rule, delete_rule_row, insert_rule, load_rule, reopen_rows,
};
use super::{Store, append_for, sets, sql_ts, users};

impl Store {
    /// Creates a rule of `user` and closes the open rows it now decides (R-16): a global rule
    /// becomes theirs, and a workspace or set it names must be theirs. Reached through
    /// [`Store::as_user`].
    ///
    /// # Errors
    /// [`StoreError::SystemActor`], [`StoreError::ExpiryNotInFuture`],
    /// [`StoreError::UnknownWorkspace`] or [`StoreError::UnknownRuleSet`] for another user's
    /// workspace or set, or a database error.
    pub(super) fn add_rule_by(
        &self,
        user: UserId,
        new: &NewRule,
        precedence: Option<Precedence>,
    ) -> Result<Rule, StoreError> {
        if new.created_by == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let rule = self.change(|tx, now, fx| {
            if new.expires_at.is_some_and(|at| at <= now) {
                return Err(StoreError::ExpiryNotInFuture);
            }
            let rule = insert_rule(tx, user, new, now, None, precedence)?;
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
    pub(super) fn peek_by(
        &self,
        user: UserId,
        workspace: &WorkspaceId,
        host: &Host,
    ) -> Option<Decision> {
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
            let rule = delete_rule_row(tx, id, now, actor)?;
            fx.push(Event::RulesChanged {});
            Ok(rule)
        })?;
        tracing::info!(rule = %id, "rule deleted");
        Ok(rule)
    }

    /// Takes back a decision made in the inbox: deletes the rule and puts the requests it closed
    /// back in the inbox (R-12 keeps the closed rows as they are, so each comes back as a new row
    /// with the same workspace, host, port, first and last time and attempt count, which puts it in
    /// its old place in the list). A request whose workspace asked for the host again meanwhile
    /// already has an open row and is not repeated. Returns the rule and the reopened rows.
    ///
    /// # Errors
    /// [`StoreError::UnknownRule`] or a database error.
    pub fn undo_decision(
        &self,
        id: RuleId,
        actor: Actor,
    ) -> Result<(Rule, Vec<PendingRow>), StoreError> {
        let (rule, reopened) = self.change(|tx, now, fx| {
            let closed = closed_by_rule(tx, id)?;
            let rule = delete_rule_row(tx, id, now, actor)?;
            let reopened = reopen_rows(tx, closed, now, fx)?;
            fx.push(Event::RulesChanged {});
            Ok((rule, reopened))
        })?;
        tracing::info!(rule = %id, reopened = reopened.len(), "decision undone");
        Ok((rule, reopened))
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

    /// Changes what a rule says (its host pattern, effect, scope and expiry, and its precedence
    /// when `edit` names one) and closes the open requests it now decides, like a new rule
    /// (R-16). The rule keeps its id, creator, creation time and switch. Writes one
    /// `rule_updated` record, and none when nothing changes. A workspace or rule set the edit
    /// names must belong to the rule's user.
    ///
    /// # Errors
    /// [`StoreError::UnknownRule`], [`StoreError::SystemActor`],
    /// [`StoreError::ExpiryNotInFuture`], [`StoreError::UnknownWorkspace`] or
    /// [`StoreError::UnknownRuleSet`] for another user's workspace or set, or a database error.
    pub fn update_rule(
        &self,
        id: RuleId,
        edit: &RuleEdit,
        actor: Actor,
    ) -> Result<Rule, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let rule = self.change(|tx, now, fx| {
            let before = load_rule(tx, id)?;
            if edit.expires_at.is_some_and(|at| at <= now) {
                return Err(StoreError::ExpiryNotInFuture);
            }
            match &edit.scope {
                Scope::Workspace(workspace) => {
                    users::require_owner(tx, workspace, before.user)?;
                }
                Scope::Set(set) => {
                    sets::require_user_set(tx, before.user, *set)?;
                }
                Scope::Global => {}
            }
            let (kind, precedence) = (
                match edit.pattern {
                    Pattern::Exact(_) => "exact",
                    Pattern::Suffix(_) => "suffix",
                },
                edit.precedence.unwrap_or(before.precedence),
            );
            tx.execute(
                "UPDATE rules SET scope = ?1, workspace_id = ?2, set_id = ?3, pattern_kind = ?4,
                                  pattern = ?5, effect = ?6, expires_at = ?7, precedence = ?8
                 WHERE id = ?9",
                params![
                    edit.scope.as_str(),
                    edit.scope.workspace().map(WorkspaceId::as_str),
                    edit.scope.set(),
                    kind,
                    edit.pattern.to_string(),
                    edit.effect.as_str(),
                    edit.expires_at.map(sql_ts),
                    precedence.get(),
                    id.0,
                ],
            )?;
            let rule = load_rule(tx, id)?;
            if rule == before {
                return Ok(rule);
            }
            append_for(
                tx,
                &AuditRecord::RuleUpdated {
                    ts: now,
                    before: RuleWire::from(&before),
                    rule: RuleWire::from(&rule),
                    actor: actor_str(actor),
                },
                rule.user,
            )?;
            close_decided_rows(tx, &rule, None, actor, now, fx)?;
            fx.push(Event::RulesChanged {});
            Ok(rule)
        })?;
        tracing::info!(rule = %id, "rule edited");
        Ok(rule)
    }

    /// Switches a rule on or off (R-65). A rule that is off never matches and keeps its place,
    /// expiry and group. Switching one on closes the open requests it now decides, like a new
    /// rule (R-16). Writes one `rule_switched` record, and none when the rule is already in that
    /// state.
    ///
    /// # Errors
    /// [`StoreError::UnknownRule`], [`StoreError::SystemActor`], or a database error.
    pub fn set_rule_enabled(
        &self,
        id: RuleId,
        enabled: bool,
        actor: Actor,
    ) -> Result<Rule, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let rule = self.change(|tx, now, fx| {
            let before = load_rule(tx, id)?;
            if before.enabled == enabled {
                return Ok(before);
            }
            tx.execute(
                "UPDATE rules SET enabled = ?1 WHERE id = ?2",
                params![i64::from(enabled), id.0],
            )?;
            let rule = load_rule(tx, id)?;
            append_for(
                tx,
                &AuditRecord::RuleSwitched {
                    ts: now,
                    kind: RuleSwitchKind::Rule,
                    id: Some(id.0),
                    set_id: rule.scope.set().map(|set| format!("user:{set}")),
                    pattern: rule.pattern.to_string(),
                    workspace_id: rule.scope.workspace().map(ToString::to_string),
                    enabled,
                    actor: actor_str(actor),
                },
                rule.user,
            )?;
            if enabled {
                close_decided_rows(tx, &rule, None, actor, now, fx)?;
            }
            fx.push(Event::RulesChanged {});
            Ok(rule)
        })?;
        tracing::info!(rule = %id, enabled, "rule switched");
        Ok(rule)
    }
}
