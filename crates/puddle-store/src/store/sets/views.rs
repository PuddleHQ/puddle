// SPDX-License-Identifier: AGPL-3.0-or-later
//! What the Rules screen reads of the sets: the list, a set's entries and a built-in set's pages.

use puddle_types::{Fill, RuleId, RuleSetId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension};

use super::index::load_hidden;
use super::user_sets::require_user_set;
use crate::catalogue::{self, BUILT_IN_SETS, BuiltInSet};
use crate::engine::{RuleIndex, default_on};
use crate::error::StoreError;
use crate::pattern::Pattern;
use crate::rule::{Effect, Precedence, Scope};
use crate::store::{Store, lock, stored_ts};
use crate::users::UserId;

/// How many entries of a built-in set the set list carries, and the most one page of
/// [`Store::built_in_entries`] returns. A big built-in list is paged and searched, never sent whole.
pub const ENTRY_PAGE: usize = 200;

/// A rule set as the Rules screen shows it: what it is, where it is on, and its entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSetInfo {
    /// `builtin:<slug>` or `user:<id>`.
    pub id: RuleSetId,
    /// Display name.
    pub name: String,
    /// What it is for.
    pub description: String,
    /// The colour it is shown in: the built-in set's own, or what the user chose for theirs
    /// (the default of the set's number when they chose none).
    pub fill: Fill,
    /// Whether it is on where nobody switched it.
    pub default_on: bool,
    /// The switch for every workspace, if set.
    pub global: Option<bool>,
    /// Workspaces that override the switch, by name.
    pub overrides: Vec<(WorkspaceId, bool)>,
    /// Its entries. A built-in set lists only its first [`ENTRY_PAGE`]; page the rest with
    /// [`Store::built_in_entries`].
    pub entries: Vec<RuleSetEntryInfo>,
    /// How many entries the set has in all (at least `entries.len()`).
    pub entry_count: usize,
    /// Built-in sets: when a Puddle update last changed the entries (R-36), else `None`.
    pub changed_at: Option<u64>,
    /// Sets the user made: when.
    pub created_at: Option<u64>,
    /// What a new rule in the set takes when none is named; `None` means 5 (R-59). Built-in sets
    /// have none: their entries sit at 0.
    pub default_precedence: Option<Precedence>,
}

impl RuleSetInfo {
    /// Whether the set is on for `workspace` (R-37).
    #[must_use]
    pub fn is_on(&self, workspace: &WorkspaceId) -> bool {
        self.overrides
            .iter()
            .find(|(s, _)| s == workspace)
            .map(|(_, on)| *on)
            .or(self.global)
            .unwrap_or(self.default_on)
    }

    /// Whether the set is on where no workspace overrides it.
    #[must_use]
    pub fn on_by_default(&self) -> bool {
        self.global.unwrap_or(self.default_on)
    }
}

/// One entry of a rule set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleSetEntryInfo {
    /// `example.com` or `.example.com`.
    pub pattern: String,
    /// Allow or deny (a built-in set's entries all share its effect).
    pub effect: Effect,
    /// What it is for (built-in entries).
    pub note: String,
    /// Whether the entry takes part in decisions (R-65).
    pub enabled: bool,
    /// The rule row, for a set the user made.
    pub rule_id: Option<RuleId>,
    /// Epoch ms after which the entry no longer counts.
    pub expires_at: Option<u64>,
    /// How it ranks: 1 to 9 for an entry of the user's set, 0 for a built-in entry (R-59).
    pub precedence: u8,
}

/// One page of a built-in set's entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPage {
    /// The entries of this page, in catalogue order.
    pub entries: Vec<RuleSetEntryInfo>,
    /// How many entries match the search in all.
    pub total: usize,
}

/// The fill a row holds. A value this Puddle can't read (hand-edited, say) is a missing choice,
/// not an error: a colour must never keep the Rules screen from loading.
pub(super) fn stored_fill(id: i64, text: Option<&str>) -> Option<Fill> {
    let text = text?;
    match serde_json::from_str(text) {
        Ok(fill) => Some(fill),
        Err(error) => {
            tracing::warn!(set = id, %error, "unreadable rule set colour; using the default");
            None
        }
    }
}

/// `entries` of the built-in `set` as `user` sees them (R-65: an entry they switched off is off).
pub(super) fn built_in_entry_infos<'a>(
    index: &RuleIndex,
    user: UserId,
    set: &BuiltInSet,
    entries: impl Iterator<Item = &'a catalogue::CatalogueEntry>,
) -> Vec<RuleSetEntryInfo> {
    let id = RuleSetId::BuiltIn(set.slug);
    entries
        .map(|entry| RuleSetEntryInfo {
            pattern: entry.pattern.to_owned(),
            effect: set.effect,
            note: entry.note.to_owned(),
            enabled: !Pattern::parse(entry.pattern).is_ok_and(|p| index.entry_off(user, id, &p)),
            rule_id: None,
            expires_at: None,
            precedence: Precedence::BUILT_IN,
        })
        .collect()
}

/// A set `user` made, with its switches from `index` and its entries.
pub(super) fn user_set_info(
    conn: &Connection,
    index: &RuleIndex,
    user: UserId,
    id: i64,
) -> Result<RuleSetInfo, StoreError> {
    let wire = require_user_set(conn, user, id)?;
    let set = RuleSetId::User(id);
    let entries: Vec<RuleSetEntryInfo> = index
        .rules()
        .iter()
        .filter(|rule| rule.scope == Scope::Set(id))
        .map(|rule| RuleSetEntryInfo {
            pattern: rule.pattern.to_string(),
            effect: rule.effect,
            note: String::new(),
            enabled: rule.enabled,
            rule_id: Some(rule.id),
            expires_at: rule.expires_at,
            precedence: rule.precedence.get(),
        })
        .collect();
    Ok(RuleSetInfo {
        id: set,
        name: wire.name,
        description: wire.description,
        fill: wire.fill.unwrap_or_else(|| Fill::default_for_user_set(id)),
        default_on: default_on(set),
        global: index.switches().global(user, set),
        overrides: index.overrides(user, set),
        entry_count: entries.len(),
        entries,
        changed_at: None,
        created_at: Some(wire.created_at),
        default_precedence: wire.default_precedence.map(Precedence::new).transpose()?,
    })
}

impl Store {
    /// Every rule set `user` sees: the built-in ones in catalogue order with `user`'s switches,
    /// then the ones `user` made, by name.
    ///
    /// # Errors
    /// A database error.
    pub fn rule_sets(&self, user: UserId) -> Result<Vec<RuleSetInfo>, StoreError> {
        let index = self.snapshot();
        let conn = lock(&self.conn);
        let hidden = load_hidden(&conn)?;
        let mut out = Vec::new();
        for set in BUILT_IN_SETS.iter().filter(|s| !hidden.contains(s.slug)) {
            let id = RuleSetId::BuiltIn(set.slug);
            let changed_at: Option<(i64, Option<i64>)> = conn
                .query_row(
                    "SELECT rowid, changed_at FROM builtin_sets_seen WHERE slug = ?1",
                    [set.slug],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let changed_at = match changed_at {
                Some((rowid, Some(t))) => Some(stored_ts("builtin_sets_seen", rowid, t)?),
                _ => None,
            };
            out.push(RuleSetInfo {
                id,
                name: set.name.to_owned(),
                description: set.description.to_owned(),
                fill: set.fill,
                default_on: set.default_on,
                global: index.switches().global(user, id),
                overrides: index.overrides(user, id),
                entries: built_in_entry_infos(
                    &index,
                    user,
                    set,
                    set.entries.iter().take(ENTRY_PAGE),
                ),
                entry_count: set.entries.len(),
                changed_at,
                created_at: None,
                default_precedence: None,
            });
        }
        let mut stmt = conn.prepare(
            "SELECT id FROM rule_sets WHERE user_id = ?1 ORDER BY name COLLATE NOCASE, id",
        )?;
        let ids = stmt
            .query_map([user.get()], |row| row.get::<_, i64>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        drop(stmt);
        for id in ids {
            out.push(user_set_info(&conn, &index, user, id)?);
        }
        Ok(out)
    }

    /// A page of the entries of the built-in set `slug`: those whose pattern or note contains
    /// `query` (ignoring case; empty matches all), from the `offset`th match, at most `limit`
    /// (capped at [`ENTRY_PAGE`]). Looks at the catalogue only, so it does not slow with the list.
    ///
    /// # Errors
    /// [`StoreError::UnknownRuleSet`] for a set this Puddle does not ship or one that is hidden.
    pub fn built_in_entries(
        &self,
        user: UserId,
        id: RuleSetId,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<EntryPage, StoreError> {
        let RuleSetId::BuiltIn(slug) = id else {
            return Err(StoreError::UnknownRuleSet(id.to_string()));
        };
        let set =
            catalogue::built_in(slug).ok_or_else(|| StoreError::UnknownRuleSet(id.to_string()))?;
        if load_hidden(&lock(&self.conn))?.contains(slug) {
            return Err(StoreError::UnknownRuleSet(id.to_string()));
        }
        let query = query.trim().to_ascii_lowercase();
        let matches = |entry: &&catalogue::CatalogueEntry| {
            query.is_empty()
                || entry.pattern.to_ascii_lowercase().contains(&query)
                || entry.note.to_ascii_lowercase().contains(&query)
        };
        let total = set.entries.iter().filter(matches).count();
        let page = set
            .entries
            .iter()
            .filter(matches)
            .skip(offset)
            .take(limit.min(ENTRY_PAGE));
        Ok(EntryPage {
            entries: built_in_entry_infos(&self.snapshot(), user, set, page),
            total,
        })
    }

    /// One rule set, as `user` sees it.
    ///
    /// # Errors
    /// [`StoreError::UnknownRuleSet`] (also for System managed, which is not a set: see
    /// [`Store::system_managed`], and for a set another user made), or a database error.
    pub fn rule_set(&self, user: UserId, id: RuleSetId) -> Result<RuleSetInfo, StoreError> {
        match id {
            RuleSetId::User(set) => {
                let index = self.snapshot();
                user_set_info(&lock(&self.conn), &index, user, set)
            }
            _ => self
                .rule_sets(user)?
                .into_iter()
                .find(|set| set.id == id)
                .ok_or_else(|| StoreError::UnknownRuleSet(id.to_string())),
        }
    }
}
