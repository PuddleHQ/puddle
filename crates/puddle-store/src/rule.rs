// SPDX-License-Identifier: AGPL-3.0-or-later
//! Rules: who they apply to, what they match, what they do, until when (`docs/spec/rules.md` §1).

use puddle_types::{RuleId, WorkspaceId};

use crate::error::StoreError;
use crate::pattern::Pattern;
use crate::users::UserId;

/// Which requests a rule applies to (R-5, R-38).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Scope {
    /// Every workspace.
    Global,
    /// One workspace.
    Workspace(WorkspaceId),
    /// An entry of a rule set the user made, by the set's id: it applies wherever the set is
    /// switched on, and ranks as a global rule (R-60).
    Set(i64),
}

impl Scope {
    /// Rank at equal pattern specificity (R-6 step 2): higher wins. `Workspace` 2; `Global` and
    /// an entry of a set 1 (R-60).
    #[must_use]
    pub fn rank(&self) -> u8 {
        match self {
            Self::Set(_) | Self::Global => 1,
            Self::Workspace(_) => 2,
        }
    }

    /// The workspace, for a workspace rule.
    #[must_use]
    pub fn workspace(&self) -> Option<&WorkspaceId> {
        match self {
            Self::Global | Self::Set(_) => None,
            Self::Workspace(id) => Some(id),
        }
    }

    /// The set, for a rule set's entry.
    #[must_use]
    pub fn set(&self) -> Option<i64> {
        match self {
            Self::Set(id) => Some(*id),
            Self::Global | Self::Workspace(_) => None,
        }
    }

    /// `global`, `workspace` or `set`, as stored.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Workspace(_) => "workspace",
            Self::Set(_) => "set",
        }
    }
}

/// How a rule ranks against the others that match (R-59, R-60): an integer from 1 to 9, higher
/// wins. Built-in entries and System managed hosts sit below it at [`Precedence::BUILT_IN`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Precedence(u8);

impl Precedence {
    /// The lowest a user's rule can have.
    pub const MIN: u8 = 1;
    /// The highest a user's rule can have.
    pub const MAX: u8 = 9;
    /// What a rule has when nothing says otherwise, and what a rule stored before precedence
    /// existed reads as.
    pub const DEFAULT: Self = Self(5);
    /// The fixed value of built-in entries and System managed hosts, below every user's rule.
    pub const BUILT_IN: u8 = 0;

    /// A precedence a user may give a rule.
    ///
    /// # Errors
    /// [`StoreError::PrecedenceOutOfRange`] outside 1 to 9.
    pub fn new(value: u8) -> Result<Self, StoreError> {
        if (Self::MIN..=Self::MAX).contains(&value) {
            Ok(Self(value))
        } else {
            Err(StoreError::PrecedenceOutOfRange(i64::from(value)))
        }
    }

    /// The number, 1 to 9.
    #[must_use]
    pub fn get(self) -> u8 {
        self.0
    }

    /// Reads a stored value; a column the schema keeps within 1 to 9 can still hold anything if
    /// the file was edited by hand.
    pub(crate) fn from_stored(value: i64) -> Result<Self, StoreError> {
        u8::try_from(value)
            .map_err(|_| StoreError::PrecedenceOutOfRange(value))
            .and_then(Self::new)
    }
}

impl Default for Precedence {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl std::fmt::Display for Precedence {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// What a matching rule does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Let the connection through.
    Allow,
    /// Refuse it.
    Deny,
}

impl Effect {
    /// `allow` or `deny`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
        }
    }
}

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Actor {
    /// The `puddle` command line.
    Cli,
    /// The desktop UI.
    Ui,
    /// The HTTP API.
    Api,
    /// Puddle itself (the sweeper, workspace deletion). Never creates rules.
    System,
    /// A workspace create, or adding a repository to a workspace: it allows the host of the
    /// repository for that workspace. Creates rules only; never decides a pending request.
    WorkspaceCreate,
}

impl Actor {
    /// `cli`, `ui`, `api`, `system` or `workspace_create`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Ui => "ui",
            Self::Api => "api",
            Self::System => "system",
            Self::WorkspaceCreate => "workspace_create",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "cli" => Self::Cli,
            "ui" => Self::Ui,
            "api" => Self::Api,
            "system" => Self::System,
            "workspace_create" => Self::WorkspaceCreate,
            _ => return None,
        })
    }
}

/// A rule as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Row id, never reused.
    pub id: RuleId,
    /// Whose it is: the workspace's user for a workspace rule, the set's for a set's entry, and
    /// the user who made it for a global rule. It decides only that user's workspaces.
    pub user: UserId,
    /// Global or one workspace.
    pub scope: Scope,
    /// Exact host or suffix.
    pub pattern: Pattern,
    /// Allow or deny.
    pub effect: Effect,
    /// Epoch ms after which the rule never matches (R-7); `None` is permanent.
    pub expires_at: Option<u64>,
    /// Epoch ms.
    pub created_at: u64,
    /// Who created it.
    pub created_by: Actor,
    /// The pending row it was approved or denied from (R-15).
    pub source_pending_id: Option<puddle_types::PendingId>,
    /// Whether the rule takes part in decisions. A rule that is off never matches and keeps
    /// everything else it has (R-65).
    pub enabled: bool,
    /// How it ranks against the other rules that match (R-59, R-60).
    pub precedence: Precedence,
}

impl Rule {
    /// Whether the rule has expired at `now` (`expires_at <= now`, R-7).
    #[must_use]
    pub fn is_expired(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }
}

/// A rule to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRule {
    /// Global or one workspace.
    pub scope: Scope,
    /// Exact host or suffix.
    pub pattern: Pattern,
    /// Allow or deny.
    pub effect: Effect,
    /// Epoch ms; `None` is permanent.
    pub expires_at: Option<u64>,
    /// Who creates it. [`Actor::System`] is refused.
    pub created_by: Actor,
}

/// What an edit of a rule says it should be now. Everything it leaves out stays: the rule's id,
/// its creator and creation time, whether it is on, and where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEdit {
    /// Global, one workspace, or a rule set of the rule's user.
    pub scope: Scope,
    /// Exact host or suffix.
    pub pattern: Pattern,
    /// Allow or deny.
    pub effect: Effect,
    /// Epoch ms; `None` is permanent.
    pub expires_at: Option<u64>,
    /// `None` keeps the rule's precedence.
    pub precedence: Option<Precedence>,
}
