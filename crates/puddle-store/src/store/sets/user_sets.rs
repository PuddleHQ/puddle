// SPDX-License-Identifier: AGPL-3.0-or-later
//! The sets the user makes: create, rename, default precedence and delete.

use puddle_types::{Event, Fill, RuleSetId};
use rusqlite::{Connection, OptionalExtension, params};

use super::views::{RuleSetInfo, stored_fill};
use crate::audit::{AuditRecord, RuleDeleteReason, RuleSetWire, RuleWire, actor_str};
use crate::catalogue::BUILT_IN_SETS;
use crate::error::StoreError;
use crate::rule::{Actor, Precedence, Rule, Scope};
use crate::store::{Store, append_for, corrupt, load_rules, sql_ts, stored_ts};
use crate::users::UserId;

/// The longest rule set name, in characters.
pub(super) const MAX_SET_NAME: usize = 64;

/// The longest rule set description, in characters.
pub(super) const MAX_SET_DESCRIPTION: usize = 500;

/// Fails unless `user` made a set with this id. Another user's set is unknown, as one that does
/// not exist.
pub(in crate::store) fn require_user_set(
    conn: &Connection,
    user: UserId,
    id: i64,
) -> Result<RuleSetWire, StoreError> {
    conn.query_row(
        "SELECT id, name, description, created_at, created_by, fill, default_precedence
         FROM rule_sets WHERE id = ?1 AND user_id = ?2",
        params![id, user.get()],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        },
    )
    .optional()?
    .ok_or_else(|| StoreError::UnknownRuleSet(RuleSetId::User(id).to_string()))
    .and_then(
        |(id, name, description, created_at, created_by, fill, default)| {
            let created_by = Actor::parse(&created_by)
                .ok_or_else(|| corrupt("rule_sets", id, format!("unknown actor {created_by:?}")))?;
            Ok(RuleSetWire {
                id,
                name,
                description,
                created_at: stored_ts("rule_sets", id, created_at)?,
                created_by: created_by.into(),
                fill: stored_fill(id, fill.as_deref()),
                default_precedence: default
                    .map(|value| {
                        Precedence::from_stored(value)
                            .map(Precedence::get)
                            .map_err(|_| corrupt("rule_sets", id, "default_precedence"))
                    })
                    .transpose()?,
            })
        },
    )
}

/// A trimmed name of 1 to [`MAX_SET_NAME`] characters without control characters, not used by
/// another set of `user` (case-insensitive), and a description of at most [`MAX_SET_DESCRIPTION`].
pub(super) fn checked_names(
    conn: &Connection,
    user: UserId,
    name: &str,
    description: &str,
    except: Option<i64>,
) -> Result<(String, String), StoreError> {
    let name = name.trim();
    let description = description.trim();
    let bad = |why: &str| Err(StoreError::RuleSetName(why.to_owned()));
    if name.is_empty() {
        return bad("a name is needed");
    }
    if name.chars().count() > MAX_SET_NAME {
        return bad("at most 64 characters");
    }
    if description.chars().count() > MAX_SET_DESCRIPTION {
        return bad("the description is longer than 500 characters");
    }
    if name
        .chars()
        .chain(description.chars())
        .any(char::is_control)
    {
        return bad("no control characters");
    }
    let taken: Option<i64> = conn
        .query_row(
            "SELECT id FROM rule_sets
             WHERE name = ?1 COLLATE NOCASE AND id IS NOT ?2 AND user_id = ?3",
            params![name, except, user.get()],
            |row| row.get(0),
        )
        .optional()?;
    if taken.is_some()
        || BUILT_IN_SETS
            .iter()
            .any(|s| s.name.eq_ignore_ascii_case(name))
    {
        return bad("another rule set has this name");
    }
    Ok((name.to_owned(), description.to_owned()))
}

impl Store {
    /// Makes an empty rule set of `user`, on by default for their workspaces (R-38). Without a
    /// `fill` the set gets the default colour of its number.
    ///
    /// # Errors
    /// [`StoreError::RuleSetName`], [`StoreError::SystemActor`], or a database error.
    pub fn create_rule_set(
        &self,
        user: UserId,
        name: &str,
        description: &str,
        fill: Option<Fill>,
        actor: Actor,
    ) -> Result<RuleSetInfo, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let id = self.change(|tx, now, fx| {
            let (name, description) = checked_names(tx, user, name, description, None)?;
            tx.execute(
                "INSERT INTO rule_sets (name, description, created_at, created_by, fill, user_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    name,
                    description,
                    sql_ts(now),
                    actor.as_str(),
                    fill.map(Fill::to_json),
                    user.get()
                ],
            )?;
            let id = tx.last_insert_rowid();
            let record = AuditRecord::RuleSetCreated {
                ts: now,
                rule_set: require_user_set(tx, user, id)?,
                actor: actor_str(actor),
            };
            append_for(tx, &record, user)?;
            fx.push(Event::RulesChanged {});
            Ok(id)
        })?;
        tracing::info!(set = id, "rule set created");
        self.rule_set(user, RuleSetId::User(id))
    }

    /// Renames a rule set `user` made, or changes its description or colour (`None` keeps the
    /// colour it has).
    ///
    /// # Errors
    /// [`StoreError::UnknownRuleSet`], [`StoreError::RuleSetName`],
    /// [`StoreError::SystemActor`], or a database error.
    pub fn update_rule_set(
        &self,
        user: UserId,
        id: i64,
        name: &str,
        description: &str,
        fill: Option<Fill>,
        actor: Actor,
    ) -> Result<RuleSetInfo, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        self.change(|tx, now, fx| {
            let before = require_user_set(tx, user, id)?;
            let (name, description) = checked_names(tx, user, name, description, Some(id))?;
            tx.execute(
                "UPDATE rule_sets SET name = ?2, description = ?3, fill = coalesce(?4, fill)
                 WHERE id = ?1",
                params![id, name, description, fill.map(Fill::to_json)],
            )?;
            let record = AuditRecord::RuleSetUpdated {
                ts: now,
                before,
                rule_set: require_user_set(tx, user, id)?,
                actor: actor_str(actor),
            };
            append_for(tx, &record, user)?;
            fx.push(Event::RulesChanged {});
            Ok(())
        })?;
        self.rule_set(user, RuleSetId::User(id))
    }

    /// Sets the precedence a new rule in a set `user` made takes when none is named, or clears
    /// it (`None`: 5). Rules already in the set keep theirs (R-59). Writes a `rule_set_updated`
    /// record, none when nothing changes.
    ///
    /// # Errors
    /// [`StoreError::UnknownRuleSet`], [`StoreError::SystemActor`], or a database error.
    pub fn set_rule_set_default_precedence(
        &self,
        user: UserId,
        id: i64,
        precedence: Option<Precedence>,
        actor: Actor,
    ) -> Result<RuleSetInfo, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        self.change(|tx, now, fx| {
            let before = require_user_set(tx, user, id)?;
            if before.default_precedence == precedence.map(Precedence::get) {
                return Ok(());
            }
            tx.execute(
                "UPDATE rule_sets SET default_precedence = ?2 WHERE id = ?1",
                params![id, precedence.map(Precedence::get)],
            )?;
            let record = AuditRecord::RuleSetUpdated {
                ts: now,
                before,
                rule_set: require_user_set(tx, user, id)?,
                actor: actor_str(actor),
            };
            append_for(tx, &record, user)?;
            fx.push(Event::RulesChanged {});
            Ok(())
        })?;
        self.rule_set(user, RuleSetId::User(id))
    }

    /// Deletes a rule set `user` made, with its entries and switches.
    ///
    /// # Errors
    /// [`StoreError::UnknownRuleSet`], [`StoreError::SystemActor`], or a database error.
    pub fn delete_rule_set(
        &self,
        user: UserId,
        id: i64,
        actor: Actor,
    ) -> Result<RuleSetInfo, StoreError> {
        if actor == Actor::System {
            return Err(StoreError::SystemActor);
        }
        let info = self.rule_set(user, RuleSetId::User(id))?;
        self.change(|tx, now, fx| {
            let set = require_user_set(tx, user, id)?;
            let entries: Vec<Rule> = load_rules(tx)?
                .into_iter()
                .filter(|rule| rule.scope == Scope::Set(id))
                .collect();
            tx.execute("DELETE FROM rules WHERE set_id = ?1", [id])?;
            for rule in &entries {
                let record = AuditRecord::RuleDeleted {
                    ts: now,
                    rule: RuleWire::from(rule),
                    reason: RuleDeleteReason::SetDeleted,
                    actor: actor_str(actor),
                };
                append_for(tx, &record, user)?;
            }
            tx.execute(
                "DELETE FROM rule_set_switches WHERE rule_set = ?1",
                [RuleSetId::User(id).to_string()],
            )?;
            tx.execute("DELETE FROM rule_sets WHERE id = ?1", [id])?;
            let record = AuditRecord::RuleSetDeleted {
                ts: now,
                rule_set: set,
                actor: actor_str(actor),
            };
            append_for(tx, &record, user)?;
            fx.push(Event::RulesChanged {});
            Ok(())
        })?;
        tracing::info!(set = id, entries = info.entries.len(), "rule set deleted");
        Ok(info)
    }
}
