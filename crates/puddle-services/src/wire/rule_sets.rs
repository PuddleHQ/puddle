// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rule sets and System managed on the wire (rules spec §7).

use puddle_store as store;
use puddle_types::{Fill, RuleSetId, WorkspaceId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{Effect, PatternKind};

/// Why Puddle allows a System managed host: a choice you made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SystemReason {
    /// You chose Microsoft's VS Code server for the browser editor.
    MicrosoftServer,
    /// The browser editor runs the bundled code-server.
    CodeServer,
    /// Direct SSH is on for the workspace.
    DirectSsh,
    /// Direct SSH is on for the workspace, for Cursor.
    DirectSshCursor,
    /// Direct SSH is on for the workspace, for `JetBrains` Gateway.
    #[serde(rename = "direct_ssh_jetbrains")]
    DirectSshJetBrains,
    /// Direct SSH is on for the workspace, for Zed.
    DirectSshZed,
    /// Puddle manages tools with mise in the workspace.
    Mise,
}

impl SystemReason {
    /// Builds the wire form from the store.
    #[must_use]
    pub fn from_store(reason: store::SystemReason) -> Option<Self> {
        Some(match reason {
            store::SystemReason::MicrosoftServer => Self::MicrosoftServer,
            store::SystemReason::CodeServer => Self::CodeServer,
            store::SystemReason::DirectSsh => Self::DirectSsh,
            store::SystemReason::DirectSshCursor => Self::DirectSshCursor,
            store::SystemReason::DirectSshJetBrains => Self::DirectSshJetBrains,
            store::SystemReason::DirectSshZed => Self::DirectSshZed,
            store::SystemReason::Mise => Self::Mise,
            _ => return None,
        })
    }

    /// The reasons named in an audit record; a name this version doesn't know is left out.
    pub fn parse_all(names: &[String]) -> Vec<Self> {
        names
            .iter()
            .filter_map(|name| store::SystemReason::parse(name))
            .filter_map(Self::from_store)
            .collect()
    }
}

/// One host Puddle allows because of your setup, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SystemManagedHost {
    /// `example.com`, or `*.example.com` for every name under it.
    pub pattern: String,
    /// What the host is for.
    pub note: String,
    /// Why it is allowed.
    pub reason: SystemReason,
    /// The reason in words.
    pub reason_text: String,
    /// The one workspace it is allowed for; `null` for every workspace.
    #[schema(required = true)]
    pub workspace: Option<WorkspaceId>,
}

impl SystemManagedHost {
    /// Builds the wire form from the store.
    #[must_use]
    pub fn from_store(host: store::SystemHost) -> Option<Self> {
        Some(Self {
            pattern: host.pattern.to_owned(),
            note: host.note.to_owned(),
            reason: SystemReason::from_store(host.reason)?,
            reason_text: host.reason.describe().to_owned(),
            workspace: host.workspace,
        })
    }
}

/// Who made a rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleSetKind {
    /// Ships with Puddle, read-only, updated with Puddle.
    BuiltIn,
    /// Made by you.
    User,
}

/// One workspace's own switch for a set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleSetOverride {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// On or off there.
    pub enabled: bool,
}

/// One entry of a rule set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleSetEntry {
    /// `example.com`, or `.example.com` for every name under it (a built-in entry is written
    /// `*.example.com`).
    pub pattern: String,
    /// Exact or suffix.
    pub pattern_kind: PatternKind,
    /// Allow or deny (a built-in set's entries all do the same; your own set may mix them).
    pub effect: Effect,
    /// What it is for (built-in sets); empty for your own entries.
    pub note: String,
    /// Whether the entry takes part in decisions. You switch a built-in entry off on its own
    /// with `PUT /api/rule-sets/{id}/entry-switch`, a rule of your own set through its rule.
    pub enabled: bool,
    /// The entry's rule, for a set you made (delete or change it through `/api/rules`).
    #[schema(required = true)]
    pub rule_id: Option<i64>,
    /// Epoch ms from which the entry no longer counts; `null` is permanent.
    #[schema(required = true)]
    pub expires_at: Option<u64>,
    /// How the entry ranks, 1 to 9; a built-in entry is fixed at 0, below every rule of yours.
    pub precedence: u8,
}

/// A rule set: where it is on, and its entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleSetView {
    /// `builtin:<slug>` or `user:<id>`.
    pub id: String,
    /// Built-in or yours.
    pub kind: RuleSetKind,
    /// Name.
    pub name: String,
    /// What it is for.
    pub description: String,
    /// The colour it is shown in: a built-in set's own, or the one you chose (a set you made
    /// without choosing has a default of its number).
    pub fill: Fill,
    /// Whether it is on where nobody switched it: built-in sets ship off, your sets start on.
    pub default_on: bool,
    /// The switch for every workspace; `null` follows `default_on`.
    #[schema(required = true)]
    pub global: Option<bool>,
    /// Workspaces with a switch of their own.
    pub overrides: Vec<RuleSetOverride>,
    /// Its entries. A built-in set lists only its first 200; `entry_count` says how many there are
    /// and `GET /api/rule-sets/{id}/entries` pages and searches them.
    pub entries: Vec<RuleSetEntry>,
    /// How many entries the set has in all.
    pub entry_count: usize,
    /// Built-in sets: epoch ms when a Puddle update last changed the entries; `null` if never.
    #[schema(required = true)]
    pub changed_at: Option<u64>,
    /// Your sets: epoch ms it was made.
    #[schema(required = true)]
    pub created_at: Option<u64>,
    /// Your sets: the precedence a new rule in the set takes when none is named, 1 to 9; `null`
    /// means 5. Changing it changes no rule already in the set.
    #[schema(required = true)]
    pub default_precedence: Option<u8>,
}

impl RuleSetView {
    /// Builds the wire form from the store.
    pub fn from_store(info: store::RuleSetInfo) -> Self {
        let store::RuleSetInfo {
            id,
            name,
            description,
            fill,
            default_on,
            global,
            overrides,
            entries,
            entry_count,
            changed_at,
            created_at,
            default_precedence,
        } = info;
        Self {
            kind: match id {
                RuleSetId::User(_) => RuleSetKind::User,
                _ => RuleSetKind::BuiltIn,
            },
            id: id.to_string(),
            name,
            description,
            fill,
            default_on,
            global,
            overrides: overrides
                .into_iter()
                .map(|(workspace, enabled)| RuleSetOverride { workspace, enabled })
                .collect(),
            entries: entries.into_iter().map(RuleSetEntry::from_store).collect(),
            entry_count,
            changed_at,
            created_at,
            default_precedence: default_precedence.map(store::Precedence::get),
        }
    }
}

impl RuleSetEntry {
    fn from_store(entry: store::RuleSetEntryInfo) -> Self {
        let store::RuleSetEntryInfo {
            pattern,
            effect,
            note,
            enabled,
            rule_id,
            expires_at,
            precedence,
        } = entry;
        Self {
            pattern_kind: if pattern.starts_with('.') || pattern.starts_with("*.") {
                PatternKind::Suffix
            } else {
                PatternKind::Exact
            },
            pattern,
            effect: effect.into(),
            note,
            enabled,
            rule_id: rule_id.map(|id| id.0),
            expires_at,
            precedence,
        }
    }
}

/// One page of a built-in set's entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleSetEntryPage {
    /// The entries of this page, in the order the set lists them.
    pub entries: Vec<RuleSetEntry>,
    /// How many entries match the search in all.
    pub total: usize,
}

impl RuleSetEntryPage {
    /// Builds the wire form from the store.
    pub fn from_store(page: store::EntryPage) -> Self {
        Self {
            entries: page
                .entries
                .into_iter()
                .map(RuleSetEntry::from_store)
                .collect(),
            total: page.total,
        }
    }
}

/// Every rule set, and the System managed hosts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleSetList {
    /// Built-in sets first, then yours by name.
    pub sets: Vec<RuleSetView>,
    /// The hosts Puddle allows because of your setup, each with its reason. Your own rules
    /// decide first: a deny of yours blocks one.
    pub system_managed: Vec<SystemManagedHost>,
}

/// A rule set to make.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewRuleSetRequest {
    /// 1 to 64 characters, not used by another set.
    pub name: String,
    /// At most 500 characters.
    #[serde(default)]
    pub description: String,
    /// The colour; left out for the default of the set's number.
    #[serde(default)]
    pub fill: Option<Fill>,
    /// 1 to 9: what a new rule in the set takes when none is named; left out for 5.
    #[serde(default)]
    pub default_precedence: Option<u8>,
}

/// A rule set's new name, description and colour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleSetUpdateRequest {
    /// 1 to 64 characters, not used by another set.
    pub name: String,
    /// At most 500 characters.
    #[serde(default)]
    pub description: String,
    /// The new colour; left out keeps the one the set has.
    #[serde(default)]
    pub fill: Option<Fill>,
    /// 1 to 9: what a new rule in the set takes when none is named; left out keeps the one the
    /// set has. Rules already in the set keep theirs.
    #[serde(default)]
    pub default_precedence: Option<u8>,
}

/// Switch a set on or off, for every workspace or for one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleSetSwitchRequest {
    /// The workspace whose own switch to set; left out or `null` for every workspace.
    #[serde(default)]
    pub workspace: Option<WorkspaceId>,
    /// On or off; `null` removes the switch, so the next level decides (the global switch, then
    /// the set's default). Required, so an empty body never removes a switch.
    #[serde(deserialize_with = "Option::deserialize")]
    #[schema(required = true)]
    pub enabled: Option<bool>,
}

/// Switch one entry of a built-in set on or off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleSetEntrySwitchRequest {
    /// The entry as the set lists it (`api.github.com`, `*.github.com`).
    pub pattern: String,
    /// `true` to switch the entry on, `false` to switch it off.
    pub enabled: bool,
}

/// What a switch did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleSetSwitched {
    /// The set as it is now.
    pub set: RuleSetView,
    /// Open requests the set now decides, closed the same way.
    pub closed: Vec<i64>,
}
