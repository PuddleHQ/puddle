// SPDX-License-Identifier: AGPL-3.0-or-later
//! Loading the rule engine's snapshot: set switches, hidden sets, System managed reasons, and the
//! record of what a Puddle update changed in the built-in sets.

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use puddle_types::{RuleSetId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, params};

use crate::audit::AuditRecord;
use crate::catalogue::{self, BUILT_IN_SETS, SystemReason};
use crate::engine::{EntryIndex, RuleIndex, SetEntry, Switches};
use crate::error::StoreError;
use crate::rule::Effect;
use crate::store::{append, corrupt, load_rules, sql_ts, users};
use crate::users::UserId;

/// The set with this wire id (`builtin:<slug>`, `user:<id>`, `system`), if it parses. A built-in
/// slug must be one this Puddle ships.
///
/// ```
/// use puddle_store::parse_rule_set;
/// use puddle_types::RuleSetId;
/// assert_eq!(parse_rule_set("user:3"), Some(RuleSetId::User(3)));
/// assert_eq!(parse_rule_set("builtin:github"), Some(RuleSetId::BuiltIn("github")));
/// assert_eq!(parse_rule_set("system"), Some(RuleSetId::System));
/// assert_eq!(parse_rule_set("builtin:nope"), None);
/// assert_eq!(parse_rule_set("user:x"), None);
/// ```
#[must_use]
pub fn parse_rule_set(text: &str) -> Option<RuleSetId> {
    if text == "system" {
        return Some(RuleSetId::System);
    }
    if let Some(slug) = text.strip_prefix("builtin:") {
        return catalogue::built_in(slug).map(|set| RuleSetId::BuiltIn(set.slug));
    }
    let id = text.strip_prefix("user:")?;
    if id.starts_with('+') {
        return None;
    }
    id.parse().ok().map(RuleSetId::User)
}

/// Every built-in set's entries, indexed once for the life of the process and shared by every
/// snapshot (S8): a rule change re-indexes only the rules and System managed entries, however
/// large a built-in list is. Hidden sets are filtered per snapshot, not here.
pub(super) fn built_in_index() -> Arc<EntryIndex> {
    static BUILT_IN: OnceLock<Arc<EntryIndex>> = OnceLock::new();
    Arc::clone(
        BUILT_IN.get_or_init(|| Arc::new(EntryIndex::new(catalogue::set_entries(BUILT_IN_SETS)))),
    )
}

/// Builds the engine's snapshot from the database and the shared built-in index.
pub(in crate::store) fn load_index(conn: &Connection) -> Result<RuleIndex, StoreError> {
    let rules = load_rules(conn)?;
    let hidden = load_hidden(conn)?;
    let mut entries = Vec::new();
    for (workspace, reason) in load_reasons(conn)? {
        for pattern in catalogue::patterns(reason.hosts()) {
            entries.push(SetEntry {
                set: RuleSetId::System,
                pattern,
                workspace: workspace.clone(),
                effect: Effect::Allow,
            });
        }
    }
    Ok(RuleIndex::new(
        rules,
        entries,
        load_switches(conn)?,
        users::load_owners(conn)?,
    )
    .with_built_in(built_in_index(), hidden.into_iter().collect()))
}

pub(super) fn load_switches(conn: &Connection) -> Result<Switches, StoreError> {
    let mut stmt = conn
        .prepare("SELECT rowid, rule_set, workspace_id, enabled, user_id FROM rule_set_switches")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    let mut switches = Switches::default();
    load_entry_switches(conn, &mut switches)?;
    for row in rows {
        let (rowid, set, workspace, enabled, user) = row?;
        // A switch of a built-in set this version no longer ships is kept but means nothing.
        let Some(set) = parse_rule_set(&set) else {
            continue;
        };
        let workspace = match workspace {
            Some(name) => match WorkspaceId::new(&name) {
                Ok(name) => Some(name),
                // A name this version can't parse is skipped like a set or reason it doesn't
                // know (R-37: such rows are ignored); the log keeps it from being silent.
                Err(e) => {
                    tracing::warn!(rowid, workspace = %name, error = %e, "rule set switch for a workspace name this version can't read; ignored");
                    continue;
                }
            },
            None => None,
        };
        switches.insert(UserId::from_stored(user), set, workspace, enabled != 0);
    }
    Ok(switches)
}

/// The built-in entries each user switched off (R-65). A row for a set or entry this version no
/// longer ships is kept but means nothing.
pub(super) fn load_entry_switches(
    conn: &Connection,
    switches: &mut Switches,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare("SELECT user_id, rule_set, pattern FROM builtin_entry_switches")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (user, set, pattern) = row?;
        if let Some(set) = parse_rule_set(&set) {
            switches.insert_entry_off(UserId::from_stored(user), set, pattern);
        }
    }
    Ok(())
}

/// The slugs of the built-in sets that are hidden now. A slug this version doesn't ship is
/// dropped when the plan is next set, and decides nothing here.
pub(super) fn load_hidden(conn: &Connection) -> Result<BTreeSet<String>, StoreError> {
    let mut stmt = conn.prepare("SELECT slug FROM hidden_builtin_sets")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(super) fn load_reasons(
    conn: &Connection,
) -> Result<Vec<(Option<WorkspaceId>, SystemReason)>, StoreError> {
    let mut stmt = conn
        .prepare("SELECT workspace_id, reason FROM system_reasons ORDER BY workspace_id, reason")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (workspace, reason) = row?;
        // Reasons are derived again from the settings at every start; one this version doesn't
        // know is dropped then.
        let Some(reason) = SystemReason::parse(&reason) else {
            continue;
        };
        let workspace = match workspace {
            Some(name) => match WorkspaceId::new(&name) {
                Ok(name) => Some(name),
                Err(_) => continue,
            },
            None => None,
        };
        out.push((workspace, reason));
    }
    Ok(out)
}

/// Records what a Puddle update changed in the built-in sets since this database last saw them
/// (R-36): one `rule_set_changed` per changed set. A set seen for the first time is only noted.
pub(in crate::store) fn note_built_in_changes(
    conn: &mut Connection,
    now: u64,
) -> Result<(), StoreError> {
    let tx = crate::store::write_tx(conn)?;
    for set in BUILT_IN_SETS {
        let current: BTreeSet<String> = catalogue::patterns(set.entries)
            .iter()
            .map(ToString::to_string)
            .collect();
        let stored: Option<(i64, String)> = tx
            .query_row(
                "SELECT rowid, entries FROM builtin_sets_seen WHERE slug = ?1",
                [set.slug],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let json = serde_json::to_string(&current).map_err(crate::audit::AuditError::from)?;
        match stored {
            None => {
                tx.execute(
                    "INSERT INTO builtin_sets_seen (slug, entries) VALUES (?1, ?2)",
                    params![set.slug, json],
                )?;
            }
            Some((rowid, stored)) => {
                // Reading damage as "no entries" would audit every pattern of the set as added.
                let before: BTreeSet<String> = serde_json::from_str(&stored).map_err(|e| {
                    corrupt(
                        "builtin_sets_seen",
                        rowid,
                        format!("entries of {}: {e}", set.slug),
                    )
                })?;
                if before == current {
                    continue;
                }
                tx.execute(
                    "UPDATE builtin_sets_seen SET entries = ?2, changed_at = ?3 WHERE slug = ?1",
                    params![set.slug, json, sql_ts(now)],
                )?;
                let record = AuditRecord::RuleSetChanged {
                    ts: now,
                    set_id: RuleSetId::BuiltIn(set.slug).to_string(),
                    added: current.difference(&before).cloned().collect(),
                    removed: before.difference(&current).cloned().collect(),
                };
                append(&tx, &record)?;
            }
        }
    }
    tx.commit()?;
    Ok(())
}
