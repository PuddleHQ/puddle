// SPDX-License-Identifier: AGPL-3.0-or-later
//! Storage and rules engine: SQLite schema and migrations, rules with expiry, pending requests,
//! the audit log and the sweeper. The behaviour is specified rule by rule in
//! `docs/spec/rules.md`; the tests named `rNN_*` pin each rule.
//!
//! The proxy asks for decisions through [`puddle_types::Policy`], which [`Store`] implements.
#![forbid(unsafe_code)]

mod account_secrets;
mod admin;
mod audit;
mod backup;
mod catalogue;
mod clock;
mod engine;
mod environment;
mod error;
mod identity;
mod pattern;
mod pending;
mod recorder;
mod rule;
mod schema;
mod shell_init;
mod store;
mod sweeper;
#[cfg(test)]
mod test_support;
mod users;
mod workspace_git;

pub use account_secrets::{AccountSecrets, MIN_INVITE_DIGEST_BYTES};
pub use admin::{AdminAction, AdminAuditEntry, MAX_ADMIN_AUDIT_LINES, UserFacts};
pub use audit::{
    AuditError, AuditOutcome, AuditRecord, ConnectionRecord, MAX_FIELD_BYTES, MAX_LINE_BYTES,
    PendingExpiryReason, PendingWire, RequestRuleWire, RuleDeleteReason, RuleSetWire, RuleWire,
    WorkspaceOpKind, WorkspaceOpOutcome,
};
pub use backup::{BackupInfo, backup_database};
pub use catalogue::{
    BUILT_IN_SETS, BuiltInSet, CatalogueEntry, MISE_TOOLS_SLUG, SystemReason, built_in,
};
pub use clock::{Clock, ManualClock, SystemClock};
pub use environment::{
    EnvChange, EnvDraft, EnvEntry, EnvName, EnvRow, EnvScope, EnvSecret, EnvValue,
    MAX_ENTRIES_PER_SCOPE, MAX_NAME_LEN, MAX_PLAIN_VALUE_BYTES, MAX_SCOPE_BYTES, MAX_SECRET_HOSTS,
    MAX_SECRET_VALUE_CHARS, PUDDLE_OWNED_NAMES, SecretHost, SecretPlace, StartValue, StartVar,
    check_secret_value,
};
pub use error::StoreError;
pub use identity::{
    Author, Collision, CollisionWhat, Coverage, CredentialBinding, CredentialChoice, Identity,
    IdentityDraft, IdentityId, MAX_CREDENTIALS, Owner, Signing, check_attachable, collision,
    resolve,
};
pub use pattern::{Pattern, PatternError, SuffixPattern, registrable_domain};
pub use pending::{
    Decided, InboxGroup, PatternChoice, PendingRow, PendingState, Resolution, ScopeChoice,
};
pub use recorder::{ConnectionRecorder, RECORD_PROBLEM};
pub use rule::{Actor, Effect, NewRule, Rule, Scope};
pub use schema::SCHEMA_VERSION;
pub use shell_init::{MAX_SHELL_INIT_BYTES, ShellInit, ShellInitText};
pub use store::{
    AuditCursor, AuditFilter, DEFAULT_LEARNED_CAP, DEFAULT_LEARNED_TTL_SECS, InterceptionSettings,
    LearnedLimits, LearnedRow, Limits, MAX_REQUEST_RULES, MAX_SSH_AGENT_HOSTS, MAX_SSH_AGENT_KEYS,
    NewRequestRule, NewWorkspaceOp, PinnedPort, RequestRule, RuleSetEntryInfo, RuleSetInfo,
    SshAgentHost, Store, SweepReport, SystemHost, SystemPlan, UserStore, WorkspaceDeletion,
    WorkspaceInterception, WorkspaceOwners, parse_rule_set,
};
pub use sweeper::{DEFAULT_SWEEP_PERIOD, SWEEP_PROBLEM, Sweeper};
pub use users::{
    Device, DeviceId, DeviceSession, MAX_DEVICE_NAME_CHARS, MAX_USER_NAME_CHARS, Role,
    SignInMethod, User, UserId, UserName, device_name,
};
pub use workspace_git::{
    GitDefaults, GitStart, RepoEntry, RepoGitRequest, RepoRef, StartBasis, SwitchChange,
    SwitchSource, WorkspaceGit,
};
