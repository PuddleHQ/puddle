// SPDX-License-Identifier: GPL-3.0-or-later
//! Audit rules, rule sets and pending rows on the wire (docs/spec/rules.md §5).

use puddle_store as store;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::AuditRecord;

// ---------------------------------------------------------------------------------------------
// Audit (docs/spec/rules.md §5)

/// A rule as an audit record shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditRule {
    /// Row id.
    pub id: i64,
    /// `global`, `workspace` or `set`.
    pub scope: String,
    /// The workspace, for a workspace rule.
    #[schema(required = true)]
    pub workspace_id: Option<String>,
    /// `exact` or `suffix`.
    pub pattern_kind: String,
    /// `example.com` or `.example.com`.
    pub pattern: String,
    /// `allow` or `deny`.
    pub effect: String,
    /// Epoch ms, or `null` for permanent.
    #[schema(required = true)]
    pub expires_at: Option<u64>,
    /// Epoch ms.
    pub created_at: u64,
    /// `cli`, `ui` or `api`.
    pub created_by: String,
    /// The pending request it came from.
    #[schema(required = true)]
    pub source_pending_id: Option<i64>,
    /// The rule set you made, for a set's entry (`scope` is `set`).
    #[serde(default)]
    #[schema(required = true)]
    pub set_id: Option<i64>,
}

impl From<store::RuleWire> for AuditRule {
    fn from(rule: store::RuleWire) -> Self {
        let store::RuleWire {
            id,
            scope,
            workspace_id,
            pattern_kind,
            pattern,
            effect,
            expires_at,
            created_at,
            created_by,
            source_pending_id,
            set_id,
        } = rule;
        Self {
            id,
            scope,
            workspace_id,
            pattern_kind,
            pattern,
            effect,
            expires_at,
            created_at,
            created_by,
            source_pending_id,
            set_id,
        }
    }
}

/// A rule on method and path, as an audit record shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditRequestRule {
    /// Row id, numbered apart from the rules on hosts.
    pub id: i64,
    /// `global` or `workspace`.
    pub scope: String,
    /// The workspace, for a workspace rule.
    #[schema(required = true)]
    pub workspace_id: Option<String>,
    /// The host pattern: `example.com` or `*.example.com`.
    pub host: String,
    /// The methods it names, upper case; empty for every method.
    pub methods: Vec<String>,
    /// The path pattern, in canonical form.
    pub path: String,
    /// Epoch ms.
    pub created_at: u64,
    /// `cli`, `ui` or `api`.
    pub created_by: String,
}

impl From<store::RequestRuleWire> for AuditRequestRule {
    fn from(rule: store::RequestRuleWire) -> Self {
        let store::RequestRuleWire {
            id,
            scope,
            workspace_id,
            host,
            methods,
            path,
            created_at,
            created_by,
        } = rule;
        Self {
            id,
            scope,
            workspace_id,
            host,
            methods,
            path,
            created_at,
            created_by,
        }
    }
}

/// A rule set you made, as an audit record shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditRuleSet {
    /// The set's number; its id is `user:<id>`.
    pub id: i64,
    /// Name.
    pub name: String,
    /// Description.
    pub description: String,
    /// Epoch ms.
    pub created_at: u64,
    /// `cli`, `ui` or `api`.
    pub created_by: String,
    /// The colour the user chose; `null` when the set had the default of its number.
    #[schema(required = true)]
    pub fill: Option<puddle_types::Fill>,
}

impl From<store::RuleSetWire> for AuditRuleSet {
    fn from(set: store::RuleSetWire) -> Self {
        let store::RuleSetWire {
            id,
            name,
            description,
            created_at,
            created_by,
            fill,
        } = set;
        Self {
            id,
            name,
            description,
            created_at,
            created_by,
            fill,
        }
    }
}

/// A pending request as an audit record shows it. `host` comes from the guest: escape it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditPending {
    /// Row id.
    pub id: i64,
    /// The workspace.
    pub workspace_id: String,
    /// The requested host.
    pub host: String,
    /// The requested port.
    pub port: u16,
    /// Epoch ms.
    pub first_seen: u64,
    /// Epoch ms.
    pub last_seen: u64,
    /// Requests the row stands for.
    pub attempts: u64,
    /// `requested`, `allowed`, `denied` or `expired`.
    pub state: String,
    /// Epoch ms.
    #[schema(required = true)]
    pub decided_at: Option<u64>,
    /// `cli`, `ui`, `api` or `system`.
    #[schema(required = true)]
    pub decided_by: Option<String>,
    /// The deciding rule.
    #[schema(required = true)]
    pub rule_id: Option<i64>,
    /// The rule set whose entry decided it.
    #[serde(default)]
    #[schema(required = true)]
    pub rule_set: Option<String>,
}

impl From<store::PendingWire> for AuditPending {
    fn from(row: store::PendingWire) -> Self {
        let store::PendingWire {
            id,
            workspace_id,
            host,
            port,
            first_seen,
            last_seen,
            attempts,
            state,
            decided_at,
            decided_by,
            rule_id,
            rule_set,
        } = row;
        Self {
            id,
            workspace_id,
            host,
            port,
            first_seen,
            last_seen,
            attempts,
            state,
            decided_at,
            decided_by,
            rule_id,
            rule_set,
        }
    }
}

/// How the proxy handled a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionDecision {
    /// Let through.
    Allow,
    /// Refused by a deny rule.
    Deny,
    /// Refused while waiting for the user.
    Pending,
    /// Refused regardless of rules.
    Blocked,
}

impl From<puddle_types::ConnectionDecision> for ConnectionDecision {
    fn from(decision: puddle_types::ConnectionDecision) -> Self {
        match decision {
            puddle_types::ConnectionDecision::Allow => Self::Allow,
            puddle_types::ConnectionDecision::Deny => Self::Deny,
            puddle_types::ConnectionDecision::Pending => Self::Pending,
            // A decision this API doesn't know yet is a refusal.
            _ => Self::Blocked,
        }
    }
}

/// Whose connection an audit record describes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionOrigin {
    /// A workspace's connection.
    #[default]
    Workspace,
    /// Puddle's own connection on the host, such as an image pull; it has no workspace.
    Puddle,
}

impl From<puddle_types::ConnectionOrigin> for ConnectionOrigin {
    fn from(origin: puddle_types::ConnectionOrigin) -> Self {
        match origin {
            puddle_types::ConnectionOrigin::Puddle => Self::Puddle,
            // An origin this API doesn't know yet is read as a workspace's.
            _ => Self::Workspace,
        }
    }
}

impl From<ConnectionOrigin> for puddle_types::ConnectionOrigin {
    fn from(origin: ConnectionOrigin) -> Self {
        match origin {
            ConnectionOrigin::Workspace => Self::Workspace,
            ConnectionOrigin::Puddle => Self::Puddle,
        }
    }
}

/// How reaching the destination went after the proxy let a connection through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionOutcome {
    /// The connection to the destination, directly or through the company proxy, was made.
    Connected,
    /// The company proxy refused the destination, or signing in to it failed.
    UpstreamRefused,
    /// No route to the destination worked.
    ConnectFailed,
    /// Resolving or connecting took too long.
    Timeout,
}

impl From<puddle_types::ConnectionOutcome> for ConnectionOutcome {
    fn from(outcome: puddle_types::ConnectionOutcome) -> Self {
        match outcome {
            puddle_types::ConnectionOutcome::Connected => Self::Connected,
            puddle_types::ConnectionOutcome::UpstreamRefused => Self::UpstreamRefused,
            puddle_types::ConnectionOutcome::Timeout => Self::Timeout,
            // An outcome this API doesn't know yet is the general failure.
            _ => Self::ConnectFailed,
        }
    }
}

/// Why a pending request expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendingExpiryReason {
    /// No repeat for seven days. Only in records an earlier version wrote.
    Stale,
    /// Its workspace was deleted.
    WorkspaceDeleted,
}

/// Why a rule was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleDeleteReason {
    /// A user deleted it.
    User,
    /// Its workspace was deleted.
    WorkspaceDeleted,
    /// It was an entry of a rule set that was deleted.
    SetDeleted,
}

impl From<store::RuleDeleteReason> for RuleDeleteReason {
    fn from(reason: store::RuleDeleteReason) -> Self {
        match reason {
            store::RuleDeleteReason::User => Self::User,
            store::RuleDeleteReason::WorkspaceDeleted => Self::WorkspaceDeleted,
            store::RuleDeleteReason::SetDeleted => Self::SetDeleted,
        }
    }
}

/// What a `workspace_op` record is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOpKind {
    /// A workspace made.
    Create,
    /// A workspace deleted.
    Delete,
    /// A workspace's display name changed.
    Rename,
    /// A workspace started (only a failed start is recorded).
    Start,
    /// A workspace stopped (only a failed stop is recorded).
    Stop,
    /// An identity made.
    IdentityCreated,
    /// An identity changed.
    IdentityUpdated,
    /// An identity deleted.
    IdentityDeleted,
    /// The default identity changed.
    IdentityDefaultSet,
    /// A workspace's identity list changed.
    IdentitiesChanged,
    /// The order of the identities changed.
    IdentitiesReordered,
    /// A repository added to a workspace's list.
    RepoAdded,
    /// A repository's pull or push switch changed.
    RepoSwitched,
    /// A repository removed from a workspace's list.
    RepoRemoved,
    /// A zero-filled or corrupt git file (`HEAD`, the index) of a checkout rebuilt at boot.
    GitRepaired,
    /// A workspace's root disk made bigger (or the attempt failed; the detail says from what
    /// size to what size and how it took effect).
    RootDiskGrown,
}

impl From<store::WorkspaceOpKind> for WorkspaceOpKind {
    fn from(kind: store::WorkspaceOpKind) -> Self {
        use store::WorkspaceOpKind as K;
        match kind {
            K::Create => Self::Create,
            K::Delete => Self::Delete,
            K::Rename => Self::Rename,
            K::Start => Self::Start,
            K::Stop => Self::Stop,
            K::IdentityCreated => Self::IdentityCreated,
            K::IdentityUpdated => Self::IdentityUpdated,
            K::IdentityDeleted => Self::IdentityDeleted,
            K::IdentityDefaultSet => Self::IdentityDefaultSet,
            K::IdentitiesChanged => Self::IdentitiesChanged,
            K::IdentitiesReordered => Self::IdentitiesReordered,
            K::RepoAdded => Self::RepoAdded,
            K::RepoSwitched => Self::RepoSwitched,
            K::RepoRemoved => Self::RepoRemoved,
            K::GitRepaired => Self::GitRepaired,
            K::RootDiskGrown => Self::RootDiskGrown,
        }
    }
}

/// How a workspace operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOpOutcome {
    /// It went through.
    Succeeded,
    /// It did not; `detail` says why.
    Failed,
}

impl From<store::WorkspaceOpOutcome> for WorkspaceOpOutcome {
    fn from(outcome: store::WorkspaceOpOutcome) -> Self {
        match outcome {
            store::WorkspaceOpOutcome::Succeeded => Self::Succeeded,
            store::WorkspaceOpOutcome::Failed => Self::Failed,
        }
    }
}

/// One audit record with its position in the log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditEntry {
    /// Position in the log: pass the newest one as `after` to read on, or the oldest as `before`
    /// to read back.
    pub id: i64,
    /// The name the record's workspace had when the record was written; `null` for a record
    /// written before names were kept, and for one about no workspace. Show the id then.
    #[schema(required = true)]
    pub workspace_name: Option<String>,
    /// The record.
    pub record: AuditRecord,
}

/// A page of the audit log: newest first for a read without `after`, oldest first with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AuditPage {
    /// The records.
    pub entries: Vec<AuditEntry>,
    /// The `after` for reading what comes after this page, and the one to tail the log with: the
    /// newest id here, or the request's `after` (0 without one) if the page is empty.
    pub next_after: i64,
    /// The `before` for the next older page: the oldest id here; `null` when this page is the
    /// oldest matching one (it holds fewer records than `limit`) or the read ran oldest first.
    #[schema(required = true)]
    pub next_before: Option<i64>,
}

/// What an audit record says happened, for the `outcome` filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditOutcome {
    /// A connection let through, or a pending request approved.
    Allow,
    /// A connection refused by a deny rule, or a pending request denied.
    Deny,
    /// A connection refused while waiting for the user, or a pending request opened.
    Pending,
    /// A connection refused regardless of rules.
    Blocked,
    /// A pending request that expired.
    Expired,
    /// A workspace operation that went through.
    Succeeded,
    /// A workspace operation that failed.
    Failed,
}

impl From<AuditOutcome> for store::AuditOutcome {
    fn from(outcome: AuditOutcome) -> Self {
        match outcome {
            AuditOutcome::Allow => Self::Allow,
            AuditOutcome::Deny => Self::Deny,
            AuditOutcome::Pending => Self::Pending,
            AuditOutcome::Blocked => Self::Blocked,
            AuditOutcome::Expired => Self::Expired,
            AuditOutcome::Succeeded => Self::Succeeded,
            AuditOutcome::Failed => Self::Failed,
        }
    }
}

/// The record types, for the `type` filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditType {
    /// A connection.
    Connection,
    /// A new pending request.
    PendingCreated,
    /// A pending request decided.
    PendingDecided,
    /// A pending request expired.
    PendingExpired,
    /// Requests an earlier version held back over a workspace's limit. Only in old records.
    PendingSuppressed,
    /// A rule created.
    RuleCreated,
    /// A rule changed.
    RuleUpdated,
    /// A rule deleted.
    RuleDeleted,
    /// A rule expired.
    RuleExpired,
    /// A rule on method and path created.
    RequestRuleCreated,
    /// A rule on method and path deleted.
    RequestRuleDeleted,
    /// A rule set made.
    RuleSetCreated,
    /// A rule set renamed.
    RuleSetUpdated,
    /// A rule set deleted.
    RuleSetDeleted,
    /// A rule set switched.
    RuleSetSwitched,
    /// A built-in rule set changed by an update.
    RuleSetChanged,
    /// The System managed reasons changed.
    SystemManagedChanged,
    /// A workspace operation, or a change to identities or repository lists.
    WorkspaceOp,
    /// The audit trimmed to its size cap.
    AuditTrimmed,
}

impl AuditType {
    /// The record `type` tag.
    pub(crate) fn tag(self) -> &'static str {
        match self {
            Self::Connection => "connection",
            Self::PendingCreated => "pending_created",
            Self::PendingDecided => "pending_decided",
            Self::PendingExpired => "pending_expired",
            Self::PendingSuppressed => "pending_suppressed",
            Self::RuleCreated => "rule_created",
            Self::RuleUpdated => "rule_updated",
            Self::RuleDeleted => "rule_deleted",
            Self::RuleExpired => "rule_expired",
            Self::RequestRuleCreated => "request_rule_created",
            Self::RequestRuleDeleted => "request_rule_deleted",
            Self::RuleSetCreated => "rule_set_created",
            Self::RuleSetUpdated => "rule_set_updated",
            Self::RuleSetDeleted => "rule_set_deleted",
            Self::RuleSetSwitched => "rule_set_switched",
            Self::RuleSetChanged => "rule_set_changed",
            Self::SystemManagedChanged => "system_managed_changed",
            Self::WorkspaceOp => "workspace_op",
            Self::AuditTrimmed => "audit_trimmed",
        }
    }
}
