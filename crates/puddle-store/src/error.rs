// SPDX-License-Identifier: AGPL-3.0-or-later
//! The store's error type.

use puddle_types::{PendingId, RuleId, WorkspaceId};

use crate::identity::{Collision, IdentityId};

use crate::audit::AuditError;
use crate::pattern::PatternError;
use crate::pending::PendingState;
use crate::users::UserId;

/// What went wrong in the store. Messages name ids and states, never secret values.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite failed.
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    /// The database was written by a newer Puddle; it is never downgraded.
    #[error("database schema version {found} is newer than this Puddle supports ({supported})")]
    SchemaTooNew {
        /// The database's version.
        found: u32,
        /// The newest version this build knows.
        supported: u32,
    },
    /// A migration step left a row pointing at a parent that is gone, so it was rolled back.
    #[error("migration to schema version {version} left {table} rows without their {parent} row")]
    MigrationBrokeForeignKeys {
        /// The version the step was taking the database to.
        version: u32,
        /// The child table with the orphan.
        table: String,
        /// The parent table the orphan points at.
        parent: String,
    },
    /// The copy made before migrating could not be written or pruned.
    #[error("could not back up the database before migrating: {0}")]
    Backup(String),
    /// A stored row doesn't parse (edited by hand, or a bug).
    #[error("stored {table} row {id} is invalid: {reason}")]
    Corrupt {
        /// The table, such as `rules` or `pending`.
        table: &'static str,
        /// The row id.
        id: i64,
        /// What doesn't parse.
        reason: String,
    },
    /// No pending row has this id (R-17).
    #[error("no pending request {0}")]
    UnknownPending(PendingId),
    /// The pending row was already decided or expired (R-17).
    #[error("pending request {id} is already {}", state.as_str())]
    PendingNotOpen {
        /// The row.
        id: PendingId,
        /// Its current state.
        state: PendingState,
    },
    /// No rule has this id.
    #[error("no rule {0}")]
    UnknownRule(RuleId),
    /// The pattern was refused.
    #[error(transparent)]
    Pattern(#[from] PatternError),
    /// An expiry that is not in the future.
    #[error("expiry must be in the future")]
    ExpiryNotInFuture,
    /// Rules are made by a user (`cli`, `ui`, `api`), never by `system`.
    #[error("rules can only be created or changed by cli, ui or api")]
    SystemActor,
    /// No rule set has this id (`builtin:<slug>`, `user:<id>`).
    #[error("no rule set {0}")]
    UnknownRuleSet(String),
    /// System managed has no switch: it follows the user's setup (R-40).
    #[error(
        "System managed can't be switched; it follows your setup (add a deny rule to block one of its hosts)"
    )]
    NotSwitchable,
    /// A built-in set is hidden while the feature that needs it is off, so it can't be switched.
    #[error("rule set {0} is hidden while the feature it belongs to is off")]
    RuleSetHidden(String),
    /// A rule set's name is empty, too long, or already used by another set.
    #[error("rule set name: {0}")]
    RuleSetName(String),
    /// Approving into a set that is off for the request's workspace would not allow it (R-38).
    #[error("rule set {set} is off for {workspace}; switch it on there first")]
    RuleSetOff {
        /// The set.
        set: String,
        /// The workspace.
        workspace: String,
    },
    /// No identity has this id.
    #[error("no identity {0}")]
    UnknownIdentity(IdentityId),
    /// No repository row has this id on this workspace.
    #[error("no repository row {0}")]
    UnknownRepo(i64),
    /// An identity, credential, author or repository was refused; the text says why.
    #[error("{0}")]
    IdentityInvalid(String),
    /// Another identity has this label.
    #[error("another identity is already called {0}")]
    IdentityLabelTaken(String),
    /// Two identities on one workspace cover the same owner or both cover the rest of a host.
    #[error("{0}")]
    IdentityCollision(Collision),
    /// The identity is already on the workspace.
    #[error("{label} is already on this workspace")]
    IdentityAttached {
        /// Its label.
        label: String,
    },
    /// The repository is already in the workspace's table.
    #[error("{0} is already in the repository table")]
    RepoListed(String),
    /// An environment variable, secret or host was refused; the text says why.
    #[error("{0}")]
    EnvInvalid(String),
    /// No such variable in this scope.
    #[error("no variable {name} in {scope}")]
    UnknownEnv {
        /// The scope, in words.
        scope: String,
        /// The variable.
        name: String,
    },
    /// A shell init was refused; the text says why.
    #[error("{0}")]
    ShellInitInvalid(String),
    /// A rule on method and path was refused; the text says why.
    #[error("{0}")]
    RequestRuleInvalid(String),
    /// A workspace has the SSH agent on for as many hosts as it may.
    #[error("at most {0} hosts may use the SSH agent in one workspace")]
    TooManySshAgentHosts(usize),
    /// No rule on method and path has this id.
    #[error("no rule on method and path {0}")]
    UnknownRequestRule(i64),
    /// An audit record couldn't be written.
    #[error(transparent)]
    Audit(#[from] AuditError),
    /// No user has this id.
    #[error("no user {0}")]
    UnknownUser(UserId),
    /// A user name was refused; the text says why.
    #[error("{0}")]
    UserName(String),
    /// No workspace of this user has this id (another user's reads the same).
    #[error("no workspace {0}")]
    UnknownWorkspace(WorkspaceId),
    /// The workspace already belongs to another user; a workspace's user never changes.
    #[error("workspace {0} already belongs to another user")]
    WorkspaceOwned(WorkspaceId),
    /// An invite's digest was refused (too short to be a hash).
    #[error("an invite digest must be at least 16 bytes")]
    InviteInvalid,
    /// A device name was refused; the text says why.
    #[error("{0}")]
    DeviceName(String),
    /// The owner account is already made: the first visitor's chance is gone.
    #[error("the owner account is already set up")]
    SetupDone,
    /// Turning a sign-in method off would leave none, or the method does not exist yet.
    #[error("{0}")]
    SignInMethod(String),
}
