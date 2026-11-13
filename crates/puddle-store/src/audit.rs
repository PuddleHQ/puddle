// SPDX-License-Identifier: GPL-3.0-or-later
//! The audit log's wire format (`docs/spec/rules.md` §5, ADR 0002).
//!
//! Every record is one variant of [`AuditRecord`], serialised by `serde_json` only (R-23), with
//! control characters escaped and the size caps of R-26 applied in [`AuditRecord::to_line`], the
//! one place a record becomes text. Nothing secret is representable: there are no header, body,
//! query-string or credential fields (R-25).

use std::io;

use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionOrigin, ConnectionOutcome, ConnectionReason,
    LocalCategory, WorkspaceId, request_path,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pattern::Pattern;
use crate::pending::PendingRow;
use crate::rule::{Actor, Rule};

/// Longest audit line, in bytes (R-26).
pub const MAX_LINE_BYTES: usize = 4096;
/// Longest string field, in bytes, before the line cap applies (R-26).
pub const MAX_FIELD_BYTES: usize = 1024;

/// A record could not be turned into a line.
#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    /// `serde_json` failed (it can't for these types; kept rather than panicking).
    #[error("audit record did not serialise: {0}")]
    Serialise(#[from] serde_json::Error),
    /// The record is over the line cap with every string already empty.
    #[error("audit record exceeds {MAX_LINE_BYTES} bytes without its strings")]
    TooLarge,
}

/// A `connection` record (R-24). `host`, `port` and `decision` are `null` only on a
/// `reason: suppressed` summary, which carries `count` instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionRecord {
    /// Epoch ms.
    pub ts: u64,
    /// The workspace; `null` for Puddle's own connections (`origin: puddle`).
    #[serde(alias = "sandbox_id")]
    pub workspace_id: Option<String>,
    /// Whose connection it is: `workspace` or `puddle`. Absent in records written before it
    /// existed, which read as `workspace`.
    #[serde(default)]
    pub origin: ConnectionOrigin,
    /// The requested host.
    pub host: Option<String>,
    /// The requested port.
    pub port: Option<u16>,
    /// The address connected to.
    pub resolved_ip: Option<String>,
    /// The company-proxy hop that carried it (`DIRECT`, `PROXY host:port`), when an upstream
    /// route is configured. Absent in records written before it existed.
    #[serde(default)]
    pub upstream: Option<String>,
    /// What happened.
    pub decision: Option<ConnectionDecision>,
    /// How reaching the destination went after an allow (`connected`, `upstream_refused`,
    /// `connect_failed`, `timeout`); absent where nothing was attempted and in records written
    /// before it existed.
    #[serde(default)]
    pub outcome: Option<ConnectionOutcome>,
    /// Why (see [`ConnectionReason`]).
    pub reason: String,
    /// The deciding rule.
    pub rule_id: Option<i64>,
    /// The rule set whose entry decided (`system`, `builtin:<slug>`, `user:<id>`). Absent in
    /// records written before rule sets existed.
    #[serde(default)]
    pub rule_set: Option<String>,
    /// The rule on method and path that refused a request of the connection (reason
    /// `request_rule`); `method` and `path` are that request's. For a tunnel refused with
    /// `request_rule_port`, the lowest-numbered rule on the host. Its own numbering, apart from
    /// `rule_id`. Absent in records written before it existed.
    #[serde(default)]
    pub request_rule: Option<i64>,
    /// The first address of the answer that an IP rule denied, else the first the address guard refused, while the connection
    /// went to the allowed addresses only. Absent in records written before it existed.
    #[serde(default)]
    pub refused_address: Option<String>,
    /// Why `refused_address` was refused (a reason code such as `ip_rule` or `toggle:private`).
    #[serde(default)]
    pub refused_reason: Option<String>,
    /// `company_network` when the destination is in that category (allowed or refused); absent
    /// otherwise and in records written before it existed.
    #[serde(default)]
    pub category: Option<String>,
    /// The signal that put the destination in the company network (`pac_direct`, `dns_suffix`,
    /// `private_access_cgnat`, `vpn_route`); absent with `category`.
    #[serde(default)]
    pub signal: Option<String>,
    /// The pending row.
    pub pending_id: Option<i64>,
    /// The credential binding, by id.
    pub binding_id: Option<String>,
    /// Whether a credential was injected.
    pub injected: bool,
    /// The ids of the secret stand-ins that went out unchanged (wrong host, or in the path or
    /// query). Absent in records written before it existed.
    #[serde(default)]
    pub unswapped_stand_ins: Vec<String>,
    /// The ids of the stand-ins whose real values an answer from one of their hosts held, put back as
    /// the stand-in before the workspace saw it (`stand-in:secret:<NAME>` or `stand-in:login:<name>`),
    /// each once, at most 16. Absent in records written
    /// before it existed.
    #[serde(default)]
    pub scrubbed_stand_ins: Vec<String>,
    /// The kinds of credential the requests carried that Puddle did not add or swap
    /// (`github_token`, `stale_stand_in`, ...), each once; never a value. Absent in records
    /// written before it existed.
    #[serde(default)]
    pub foreign_credentials: Vec<String>,
    /// The kinds of token the answers held that Puddle does not hold (`github_token`,
    /// `jwt_with_refresh_token`, ...), each once; never a value. Absent in records written before
    /// it existed.
    #[serde(default)]
    pub answer_credentials: Vec<String>,
    /// HTTP method: plain-HTTP requests, tunnels that carry plain HTTP, terminated hosts.
    pub method: Option<String>,
    /// HTTP path without query string, where `method` is set.
    pub path: Option<String>,
    /// Whether `path` was cut to fit (R-26).
    pub path_truncated: bool,
    /// Bytes from the guest.
    pub bytes_up: u64,
    /// Bytes to the guest.
    pub bytes_down: u64,
    /// Records summarised, on a `suppressed` summary.
    pub count: Option<u64>,
}

impl ConnectionRecord {
    pub(crate) fn from_event(ts: u64, event: &ConnectionEvent) -> Self {
        let (method, path) = event.http.as_ref().map_or((None, None), |http| {
            (Some(http.method().to_owned()), Some(http.path().to_owned()))
        });
        Self {
            ts,
            workspace_id: event.workspace.as_ref().map(ToString::to_string),
            origin: event.origin,
            host: Some(event.host.to_string()),
            port: Some(event.port),
            resolved_ip: event.resolved_ip.map(|ip| ip.to_string()),
            upstream: event.upstream.clone(),
            decision: Some(event.decision),
            outcome: event.outcome,
            reason: event.reason.to_string(),
            refused_address: event.refused_address.map(|(ip, _)| ip.to_string()),
            refused_reason: event.refused_address.map(|(_, why)| why.code().to_owned()),
            category: event
                .company_network
                .map(|_| LocalCategory::CompanyNetwork.key().to_owned()),
            signal: event.company_network.map(|s| s.code().to_owned()),
            rule_id: event.rule_id.map(|id| id.0),
            rule_set: event.rule_set.map(|set| set.to_string()),
            request_rule: event.request_rule.map(|id| id.0),
            pending_id: event.pending_id.map(|id| id.0),
            binding_id: event.binding_id.clone(),
            injected: event.injected,
            unswapped_stand_ins: event.unswapped_stand_ins.clone(),
            scrubbed_stand_ins: event.scrubbed_stand_ins.clone(),
            foreign_credentials: event.foreign_credentials.clone(),
            answer_credentials: event.answer_credentials.clone(),
            method,
            path,
            path_truncated: false,
            bytes_up: event.bytes_up,
            bytes_down: event.bytes_down,
            count: None,
        }
    }

    /// The `suppressed` summary for a workspace, or for Puddle's own connections when `workspace` is
    /// `None`.
    pub(crate) fn suppressed_summary(ts: u64, workspace: Option<&WorkspaceId>, count: u64) -> Self {
        Self::summary(ts, workspace, ConnectionReason::Suppressed, count)
    }

    /// The `merged` summary of the oldest records of waiting requests a workspace had too many of.
    pub(crate) fn merged_summary(ts: u64, workspace: &WorkspaceId, count: u64) -> Self {
        Self::summary(ts, Some(workspace), ConnectionReason::Merged, count)
    }

    fn summary(
        ts: u64,
        workspace: Option<&WorkspaceId>,
        reason: ConnectionReason,
        count: u64,
    ) -> Self {
        Self {
            ts,
            workspace_id: workspace.map(ToString::to_string),
            origin: if workspace.is_some() {
                ConnectionOrigin::Workspace
            } else {
                ConnectionOrigin::Puddle
            },
            host: None,
            port: None,
            resolved_ip: None,
            upstream: None,
            decision: None,
            outcome: None,
            reason: reason.to_string(),
            refused_address: None,
            refused_reason: None,
            category: None,
            signal: None,
            rule_id: None,
            rule_set: None,
            request_rule: None,
            pending_id: None,
            binding_id: None,
            injected: false,
            unswapped_stand_ins: Vec::new(),
            scrubbed_stand_ins: Vec::new(),
            foreign_credentials: Vec::new(),
            answer_credentials: Vec::new(),
            method: None,
            path: None,
            path_truncated: false,
            bytes_up: 0,
            bytes_down: 0,
            count: Some(count),
        }
    }
}

/// Reads a stored rule scope, mapping the name `sandbox` that older records carry.
fn scope_name<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let scope = String::deserialize(d)?;
    Ok(if scope == "sandbox" {
        "workspace".to_owned()
    } else {
        scope
    })
}

/// A rule as it appears in the audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleWire {
    /// Row id.
    pub id: i64,
    /// `global`, `workspace` or `set`. Records written before the rename read `sandbox` as
    /// `workspace`.
    #[serde(deserialize_with = "scope_name")]
    pub scope: String,
    /// The workspace, for a workspace rule.
    #[serde(alias = "sandbox_id")]
    pub workspace_id: Option<String>,
    /// The rule set the user made, for a set's entry. Absent in records written before rule
    /// sets existed.
    #[serde(default)]
    pub set_id: Option<i64>,
    /// `exact` or `suffix`.
    pub pattern_kind: String,
    /// `example.com` or `.example.com`.
    pub pattern: String,
    /// `allow` or `deny`.
    pub effect: String,
    /// Epoch ms, or `null` for permanent.
    pub expires_at: Option<u64>,
    /// Epoch ms.
    pub created_at: u64,
    /// `cli`, `ui` or `api`.
    pub created_by: String,
    /// The pending row it came from.
    pub source_pending_id: Option<i64>,
}

impl From<&Rule> for RuleWire {
    fn from(rule: &Rule) -> Self {
        Self {
            id: rule.id.0,
            scope: rule.scope.as_str().to_owned(),
            workspace_id: rule.scope.workspace().map(ToString::to_string),
            set_id: rule.scope.set(),
            pattern_kind: match rule.pattern {
                Pattern::Exact(_) => "exact",
                Pattern::Suffix(_) => "suffix",
            }
            .to_owned(),
            pattern: rule.pattern.to_string(),
            effect: rule.effect.as_str().to_owned(),
            expires_at: rule.expires_at,
            created_at: rule.created_at,
            created_by: rule.created_by.as_str().to_owned(),
            source_pending_id: rule.source_pending_id.map(|id| id.0),
        }
    }
}

/// A pending row as it appears in the audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingWire {
    /// Row id.
    pub id: i64,
    /// The workspace.
    #[serde(alias = "sandbox_id")]
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
    pub decided_at: Option<u64>,
    /// `cli`, `ui`, `api` or `system`.
    pub decided_by: Option<String>,
    /// The deciding rule.
    pub rule_id: Option<i64>,
    /// The rule set whose entry decided it. Absent in records written before rule sets existed.
    #[serde(default)]
    pub rule_set: Option<String>,
}

impl From<&PendingRow> for PendingWire {
    fn from(row: &PendingRow) -> Self {
        Self {
            id: row.id.0,
            workspace_id: row.workspace.to_string(),
            host: row.host.to_string(),
            port: row.port,
            first_seen: row.first_seen,
            last_seen: row.last_seen,
            attempts: row.attempts,
            state: row.state.as_str().to_owned(),
            decided_at: row.decided_at,
            decided_by: row.decided_by.map(|a| a.as_str().to_owned()),
            rule_id: row.rule_id.map(|id| id.0),
            rule_set: row.rule_set.clone(),
        }
    }
}

/// Why a pending row expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingExpiryReason {
    /// No repeat for seven days. Earlier versions wrote this; requests no longer expire by age
    /// (R-49), so it only appears in old records.
    Stale,
    /// Its workspace was deleted (R-21).
    #[serde(alias = "sandbox_deleted")]
    WorkspaceDeleted,
}

/// Why a rule was deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleDeleteReason {
    /// A user deleted it.
    User,
    /// Its workspace was deleted (R-21).
    #[serde(alias = "sandbox_deleted")]
    WorkspaceDeleted,
    /// It was an entry of a rule set the user deleted (R-38).
    SetDeleted,
}

/// A rule on method and path as it appears in the audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestRuleWire {
    /// Row id, numbered apart from the rules on hosts.
    pub id: i64,
    /// `global` or `workspace`.
    pub scope: String,
    /// The workspace, for a workspace rule.
    pub workspace_id: Option<String>,
    /// The host pattern (`example.com` or `*.example.com`).
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

/// A rule set the user made, as it appears in the audit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleSetWire {
    /// Row id; the set's wire id is `user:<id>`.
    pub id: i64,
    /// Display name.
    pub name: String,
    /// What it is for.
    pub description: String,
    /// Epoch ms.
    pub created_at: u64,
    /// `cli`, `ui` or `api`.
    pub created_by: String,
    /// The colour the user chose; `None` when the set has the default of its number (and in
    /// records written before sets had a colour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill: Option<puddle_types::Fill>,
}

/// What a `workspace_op` record is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOpKind {
    /// A workspace made.
    Create,
    /// A workspace deleted, with its volume (or, when the volume was already gone, without it).
    Delete,
    /// A workspace's display name changed.
    Rename,
    /// A workspace started. Only a failed start is recorded.
    Start,
    /// A workspace stopped. Only a failed stop is recorded.
    Stop,
    /// An identity made.
    IdentityCreated,
    /// An identity's label, author or credentials replaced.
    IdentityUpdated,
    /// An identity deleted.
    IdentityDeleted,
    /// The default identity changed.
    IdentityDefaultSet,
    /// A workspace's identity list changed (one attached, one taken off, or a new order).
    IdentitiesChanged,
    /// The user's order of the identities changed.
    IdentitiesReordered,
    /// A repository added to a workspace's list.
    RepoAdded,
    /// A repository's pull or push switch changed.
    RepoSwitched,
    /// A repository removed from a workspace's list.
    RepoRemoved,
    /// A zero-filled or corrupt git file (`HEAD`, the index) of a checkout rebuilt at boot.
    GitRepaired,
    /// A workspace's root disk made bigger, or the attempt failed; `detail` says from what size
    /// to what size and how it took effect.
    RootDiskGrown,
}

/// How a workspace operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceOpOutcome {
    /// It went through.
    Succeeded,
    /// It did not; `detail` says why.
    Failed,
}

/// One audit record (R-24). Internally tagged: `{"type": "rule_created", ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditRecord {
    /// A connection the proxy handled (written by the proxy).
    Connection(ConnectionRecord),
    /// A new pending row.
    PendingCreated {
        /// Epoch ms.
        ts: u64,
        /// The row.
        pending: PendingWire,
    },
    /// A pending row approved or denied, by the user or by a rule that now covers it (R-16).
    PendingDecided {
        /// Epoch ms.
        ts: u64,
        /// The row as decided.
        pending: PendingWire,
    },
    /// A pending row expired.
    PendingExpired {
        /// Epoch ms.
        ts: u64,
        /// The row as expired.
        pending: PendingWire,
        /// Workspace deleted (or, in old records, stale).
        reason: PendingExpiryReason,
    },
    /// Requests an earlier version held back over a workspace's limit and wrote no row for.
    /// Nothing writes this any more (R-44); it stays so old records still read.
    PendingSuppressed {
        /// Epoch ms.
        ts: u64,
        /// The workspace.
        #[serde(alias = "sandbox_id")]
        workspace_id: String,
        /// Requests suppressed since the previous record.
        count: u64,
    },
    /// A rule created.
    RuleCreated {
        /// Epoch ms.
        ts: u64,
        /// The rule.
        rule: RuleWire,
    },
    /// A rule changed.
    RuleUpdated {
        /// Epoch ms.
        ts: u64,
        /// The rule before.
        before: RuleWire,
        /// The rule after.
        rule: RuleWire,
        /// Who changed it.
        actor: String,
    },
    /// A rule deleted.
    RuleDeleted {
        /// Epoch ms.
        ts: u64,
        /// The rule as it was.
        rule: RuleWire,
        /// User or workspace deletion.
        reason: RuleDeleteReason,
        /// Who deleted it.
        actor: String,
    },
    /// A rule removed by the sweeper after it expired (R-19).
    RuleExpired {
        /// Epoch ms.
        ts: u64,
        /// The whole rule.
        rule: RuleWire,
    },
    /// A rule on method and path created (R-49).
    RequestRuleCreated {
        /// Epoch ms.
        ts: u64,
        /// The rule.
        rule: RequestRuleWire,
    },
    /// A rule on method and path deleted.
    RequestRuleDeleted {
        /// Epoch ms.
        ts: u64,
        /// The rule as it was.
        rule: RequestRuleWire,
        /// User or workspace deletion.
        reason: RuleDeleteReason,
        /// Who deleted it.
        actor: String,
    },
    /// A rule set made by the user (R-43).
    RuleSetCreated {
        /// Epoch ms.
        ts: u64,
        /// The set.
        rule_set: RuleSetWire,
        /// Who made it.
        actor: String,
    },
    /// A rule set renamed or described anew.
    RuleSetUpdated {
        /// Epoch ms.
        ts: u64,
        /// The set before.
        before: RuleSetWire,
        /// The set after.
        rule_set: RuleSetWire,
        /// Who changed it.
        actor: String,
    },
    /// A rule set deleted, with its entries (each also gets a `rule_deleted`).
    RuleSetDeleted {
        /// Epoch ms.
        ts: u64,
        /// The set as it was.
        rule_set: RuleSetWire,
        /// Who deleted it.
        actor: String,
    },
    /// A rule set switched on or off, or back to following the next level (R-37).
    RuleSetSwitched {
        /// Epoch ms.
        ts: u64,
        /// `builtin:<slug>` or `user:<id>`.
        set_id: String,
        /// The workspace whose override changed, or `null` for every workspace.
        #[serde(alias = "sandbox_id")]
        workspace_id: Option<String>,
        /// On, off, or `null` to follow the next level.
        enabled: Option<bool>,
        /// Who switched it.
        actor: String,
    },
    /// A Puddle update changed a built-in set's entries (R-36).
    RuleSetChanged {
        /// Epoch ms.
        ts: u64,
        /// `builtin:<slug>`.
        set_id: String,
        /// Patterns the update added.
        added: Vec<String>,
        /// Patterns the update removed.
        removed: Vec<String>,
    },
    /// The reasons Puddle allows System managed hosts changed, because the user's setup did
    /// (R-41).
    SystemManagedChanged {
        /// Epoch ms.
        ts: u64,
        /// The workspace, or `null` for every workspace.
        #[serde(alias = "sandbox_id")]
        workspace_id: Option<String>,
        /// Reasons that now apply (`microsoft_server`, `code_server`, `direct_ssh`, `direct_ssh_cursor`, `direct_ssh_jetbrains`, `direct_ssh_zed`, `mise`).
        added: Vec<String>,
        /// Reasons that no longer apply.
        removed: Vec<String>,
    },
    /// A workspace operation, or a change to the identities and repository lists a workspace
    /// uses: who asked, which workspace and how it ended.
    WorkspaceOp {
        /// Epoch ms.
        ts: u64,
        /// What was done.
        op: WorkspaceOpKind,
        /// The workspace; `null` for a change that belongs to no workspace (an identity).
        workspace_id: Option<String>,
        /// How it ended.
        outcome: WorkspaceOpOutcome,
        /// Who asked: `cli`, `ui`, `api` or `system`.
        actor: String,
        /// A short plain sentence: why it failed, or what the operation touched (the new name,
        /// the repository, the identity's label). Never a secret. Absent when there is nothing
        /// to add.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Oldest records deleted to keep the audit under its cap (R-26).
    AuditTrimmed {
        /// Epoch ms.
        ts: u64,
        /// Records deleted.
        deleted_records: u64,
        /// `ts` of the oldest record left, `null` if none.
        oldest_ts_kept: Option<u64>,
    },
}

/// What a record says happened, for filtering (`GET /api/audit?outcome=`). Only records that
/// are about a decision have one: connections, and pending requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// A pending request that expired without a decision.
    Expired,
    /// A workspace operation that went through.
    Succeeded,
    /// A workspace operation that failed.
    Failed,
}

impl AuditOutcome {
    /// Every outcome.
    pub const ALL: [Self; 7] = [
        Self::Allow,
        Self::Deny,
        Self::Pending,
        Self::Blocked,
        Self::Expired,
        Self::Succeeded,
        Self::Failed,
    ];

    /// The `snake_case` name stored in the `audit.outcome` column and used on the wire.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::Pending => "pending",
            Self::Blocked => "blocked",
            Self::Expired => "expired",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
        }
    }
}

impl AuditRecord {
    /// Every `type` tag, in declaration order.
    pub const KINDS: [&'static str; 19] = [
        "connection",
        "pending_created",
        "pending_decided",
        "pending_expired",
        "pending_suppressed",
        "rule_created",
        "rule_updated",
        "rule_deleted",
        "rule_expired",
        "request_rule_created",
        "request_rule_deleted",
        "rule_set_created",
        "rule_set_updated",
        "rule_set_deleted",
        "rule_set_switched",
        "rule_set_changed",
        "system_managed_changed",
        "workspace_op",
        "audit_trimmed",
    ];

    /// The host the record is about, lower case: a connection's or pending request's host, or a
    /// rule's pattern (`.example.com` for a suffix). Stored in a column so `host_contains` needs
    /// no JSON parsing. The migration that added the column derives the same value in SQL
    /// (`schema.rs`); a test keeps the two equal.
    #[must_use]
    pub fn host(&self) -> Option<String> {
        let host = match self {
            Self::Connection(record) => record.host.as_deref()?,
            Self::PendingCreated { pending, .. }
            | Self::PendingDecided { pending, .. }
            | Self::PendingExpired { pending, .. } => &pending.host,
            Self::RuleCreated { rule, .. }
            | Self::RuleUpdated { rule, .. }
            | Self::RuleDeleted { rule, .. }
            | Self::RuleExpired { rule, .. } => &rule.pattern,
            Self::RequestRuleCreated { rule, .. } | Self::RequestRuleDeleted { rule, .. } => {
                &rule.host
            }
            Self::PendingSuppressed { .. }
            | Self::RuleSetCreated { .. }
            | Self::RuleSetUpdated { .. }
            | Self::RuleSetDeleted { .. }
            | Self::RuleSetSwitched { .. }
            | Self::RuleSetChanged { .. }
            | Self::SystemManagedChanged { .. }
            | Self::WorkspaceOp { .. }
            | Self::AuditTrimmed { .. } => return None,
        };
        Some(host.to_ascii_lowercase())
    }

    /// The text the audit search box matches, lower case (ASCII), one part per line: a
    /// connection's host, method, path and reason; a rule on method and path's host and path;
    /// otherwise the record's [`host`](Self::host). Stored in a column so `q` needs no JSON
    /// parsing. The migration that added the column derives the same value in SQL
    /// (`schema.rs`); a test keeps the two equal.
    #[must_use]
    pub fn search_text(&self) -> Option<String> {
        let parts: Vec<Option<String>> = match self {
            Self::Connection(r) => vec![
                r.host.clone(),
                r.method.clone(),
                r.path.clone(),
                Some(r.reason.clone()),
                r.category.clone(),
            ],
            Self::RequestRuleCreated { rule, .. } | Self::RequestRuleDeleted { rule, .. } => {
                vec![Some(rule.host.clone()), Some(rule.path.clone())]
            }
            _ => vec![self.host()],
        };
        let text = parts
            .into_iter()
            .flatten()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
            .to_ascii_lowercase();
        (!text.is_empty()).then_some(text)
    }

    /// Whose connection the record describes: `connection` records only (every other record
    /// has none). A Puddle connection is the one with no workspace, which is how the `origin`
    /// filter finds it without a column of its own.
    #[must_use]
    pub fn origin(&self) -> Option<ConnectionOrigin> {
        match self {
            Self::Connection(record) => Some(record.origin),
            _ => None,
        }
    }

    /// The record's [`AuditOutcome`], if it has one.
    #[must_use]
    pub fn outcome(&self) -> Option<AuditOutcome> {
        match self {
            Self::Connection(record) => record.decision.map(|d| match d {
                ConnectionDecision::Allow => AuditOutcome::Allow,
                ConnectionDecision::Deny => AuditOutcome::Deny,
                ConnectionDecision::Pending => AuditOutcome::Pending,
                // `ConnectionDecision` is non-exhaustive; a new decision is a refusal until
                // this match learns it.
                _ => AuditOutcome::Blocked,
            }),
            Self::PendingCreated { .. } => Some(AuditOutcome::Pending),
            Self::PendingDecided { pending, .. } => match pending.state.as_str() {
                "allowed" => Some(AuditOutcome::Allow),
                "denied" => Some(AuditOutcome::Deny),
                _ => None,
            },
            Self::PendingExpired { .. } => Some(AuditOutcome::Expired),
            Self::WorkspaceOp { outcome, .. } => Some(match outcome {
                WorkspaceOpOutcome::Succeeded => AuditOutcome::Succeeded,
                WorkspaceOpOutcome::Failed => AuditOutcome::Failed,
            }),
            Self::PendingSuppressed { .. }
            | Self::RuleCreated { .. }
            | Self::RuleUpdated { .. }
            | Self::RuleDeleted { .. }
            | Self::RuleExpired { .. }
            | Self::RequestRuleCreated { .. }
            | Self::RequestRuleDeleted { .. }
            | Self::RuleSetCreated { .. }
            | Self::RuleSetUpdated { .. }
            | Self::RuleSetDeleted { .. }
            | Self::RuleSetSwitched { .. }
            | Self::RuleSetChanged { .. }
            | Self::SystemManagedChanged { .. }
            | Self::AuditTrimmed { .. } => None,
        }
    }

    /// The `type` tag.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Connection(_) => "connection",
            Self::PendingCreated { .. } => "pending_created",
            Self::PendingDecided { .. } => "pending_decided",
            Self::PendingExpired { .. } => "pending_expired",
            Self::PendingSuppressed { .. } => "pending_suppressed",
            Self::RuleCreated { .. } => "rule_created",
            Self::RuleUpdated { .. } => "rule_updated",
            Self::RuleDeleted { .. } => "rule_deleted",
            Self::RuleExpired { .. } => "rule_expired",
            Self::RequestRuleCreated { .. } => "request_rule_created",
            Self::RequestRuleDeleted { .. } => "request_rule_deleted",
            Self::RuleSetCreated { .. } => "rule_set_created",
            Self::RuleSetUpdated { .. } => "rule_set_updated",
            Self::RuleSetDeleted { .. } => "rule_set_deleted",
            Self::RuleSetSwitched { .. } => "rule_set_switched",
            Self::RuleSetChanged { .. } => "rule_set_changed",
            Self::SystemManagedChanged { .. } => "system_managed_changed",
            Self::WorkspaceOp { .. } => "workspace_op",
            Self::AuditTrimmed { .. } => "audit_trimmed",
        }
    }

    /// Epoch ms.
    #[must_use]
    pub fn ts(&self) -> u64 {
        match self {
            Self::Connection(record) => record.ts,
            Self::PendingCreated { ts, .. }
            | Self::PendingDecided { ts, .. }
            | Self::PendingExpired { ts, .. }
            | Self::PendingSuppressed { ts, .. }
            | Self::RuleCreated { ts, .. }
            | Self::RuleUpdated { ts, .. }
            | Self::RuleDeleted { ts, .. }
            | Self::RuleExpired { ts, .. }
            | Self::RequestRuleCreated { ts, .. }
            | Self::RequestRuleDeleted { ts, .. }
            | Self::RuleSetCreated { ts, .. }
            | Self::RuleSetUpdated { ts, .. }
            | Self::RuleSetDeleted { ts, .. }
            | Self::RuleSetSwitched { ts, .. }
            | Self::RuleSetChanged { ts, .. }
            | Self::SystemManagedChanged { ts, .. }
            | Self::WorkspaceOp { ts, .. }
            | Self::AuditTrimmed { ts, .. } => *ts,
        }
    }

    /// The workspace the record is about, if any (indexed for per-workspace reads).
    #[must_use]
    pub fn workspace_id(&self) -> Option<&str> {
        match self {
            Self::Connection(record) => record.workspace_id.as_deref(),
            Self::PendingCreated { pending, .. }
            | Self::PendingDecided { pending, .. }
            | Self::PendingExpired { pending, .. } => Some(&pending.workspace_id),
            Self::PendingSuppressed { workspace_id, .. } => Some(workspace_id),
            Self::RuleCreated { rule, .. }
            | Self::RuleUpdated { rule, .. }
            | Self::RuleDeleted { rule, .. }
            | Self::RuleExpired { rule, .. } => rule.workspace_id.as_deref(),
            Self::RequestRuleCreated { rule, .. } | Self::RequestRuleDeleted { rule, .. } => {
                rule.workspace_id.as_deref()
            }
            Self::RuleSetSwitched { workspace_id, .. }
            | Self::SystemManagedChanged { workspace_id, .. }
            | Self::WorkspaceOp { workspace_id, .. } => workspace_id.as_deref(),
            Self::RuleSetCreated { .. }
            | Self::RuleSetUpdated { .. }
            | Self::RuleSetDeleted { .. }
            | Self::RuleSetChanged { .. }
            | Self::AuditTrimmed { .. } => None,
        }
    }

    /// The record as one JSONL line (no newline), at most [`MAX_LINE_BYTES`].
    ///
    /// Every string longer than [`MAX_FIELD_BYTES`] is cut to it; `path` is also stripped of any
    /// query string and marks `path_truncated`. If the line is still too long, the longest
    /// string is halved until it fits. Control characters (C0, DEL, C1) and U+2028/U+2029 come
    /// out as `\uXXXX`.
    ///
    /// # Errors
    /// [`AuditError`] if the record can't be serialised or can't fit.
    pub fn to_line(&self) -> Result<String, AuditError> {
        let mut value = serde_json::to_value(self)?;
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .map(|p| request_path(p).to_owned());
        if let (Some(path), Some(slot)) = (path, value.get_mut("path")) {
            *slot = Value::String(path);
        }
        let mut cut = Vec::new();
        cap_strings(&mut value, MAX_FIELD_BYTES, "", &mut cut);
        let mut line = encode(&value)?;
        while line.len() > MAX_LINE_BYTES {
            let Some((_, pointer)) = longest_string(&value, "") else {
                return Err(AuditError::TooLarge);
            };
            if let Some(Value::String(s)) = value.pointer_mut(&pointer) {
                let keep = s.floor_char_boundary(s.len() / 2);
                s.truncate(keep);
            }
            cut.push(pointer);
            line = encode(&value)?;
        }
        if cut.iter().any(|p| p == "/path")
            && let Some(flag) = value.get_mut("path_truncated")
        {
            *flag = Value::Bool(true);
            line = encode(&value)?;
        }
        Ok(line)
    }
}

/// Cuts every string over `max` bytes, recording the JSON pointer of each one cut.
fn cap_strings(value: &mut Value, max: usize, pointer: &str, cut: &mut Vec<String>) {
    match value {
        Value::String(s) if s.len() > max => {
            let keep = s.floor_char_boundary(max);
            s.truncate(keep);
            cut.push(pointer.to_owned());
        }
        Value::Object(map) => {
            for (key, child) in map {
                cap_strings(child, max, &format!("{pointer}/{key}"), cut);
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                cap_strings(child, max, &format!("{pointer}/{index}"), cut);
            }
        }
        _ => {}
    }
}

/// The JSON pointer and encoded length of the non-empty string whose encoding is longest.
fn longest_string(value: &Value, pointer: &str) -> Option<(usize, String)> {
    match value {
        Value::String(s) if !s.is_empty() => Some((encoded_len(s), pointer.to_owned())),
        Value::Object(map) => map
            .iter()
            .filter_map(|(key, child)| longest_string(child, &format!("{pointer}/{key}")))
            .max_by_key(|(len, _)| *len),
        Value::Array(items) => items
            .iter()
            .enumerate()
            .filter_map(|(index, child)| longest_string(child, &format!("{pointer}/{index}")))
            .max_by_key(|(len, _)| *len),
        _ => None,
    }
}

fn encoded_len(s: &str) -> usize {
    s.chars()
        .map(|c| if needs_escape(c) { 6 } else { c.len_utf8() })
        .sum()
}

fn needs_escape(c: char) -> bool {
    c.is_control() || c == '"' || c == '\\' || c == '\u{2028}' || c == '\u{2029}'
}

fn encode(value: &Value) -> Result<String, AuditError> {
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut out, EscapingFormatter);
    value.serialize(&mut ser)?;
    // serde_json only ever writes UTF-8.
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// `serde_json`'s compact formatter, plus `\uXXXX` for DEL, C1 controls and U+2028/U+2029,
/// which `serde_json` leaves raw. C0 controls are already escaped before they get here.
struct EscapingFormatter;

impl serde_json::ser::Formatter for EscapingFormatter {
    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut start = 0;
        for (index, c) in fragment.char_indices() {
            if needs_escape(c) {
                writer.write_all(fragment.get(start..index).unwrap_or_default().as_bytes())?;
                write!(writer, "\\u{:04x}", u32::from(c))?;
                start = index + c.len_utf8();
            }
        }
        writer.write_all(fragment.get(start..).unwrap_or_default().as_bytes())
    }
}

/// Per-workspace limit on `connection` records (R-26): at most `limit` per wall-clock second; the
/// excess is counted and written as one summary record when the second is over.
#[derive(Debug, Default)]
pub(crate) struct ConnectionWindow {
    second: u64,
    written: u32,
    excess: u64,
    /// A summary whose audit write failed: `(second_start_ms, count)`, handed out again by the
    /// next [`ConnectionWindow::roll`].
    owed: Option<(u64, u64)>,
}

impl ConnectionWindow {
    /// Whether a record at `now` may be written, and a summary `(second_start_ms, count)` for a
    /// finished second that had excess.
    pub(crate) fn admit(&mut self, now: u64, limit: u32) -> (bool, Option<(u64, u64)>) {
        let finished = self.roll(now);
        if self.written < limit {
            self.written += 1;
            (true, finished)
        } else {
            self.excess += 1;
            (false, finished)
        }
    }

    /// The summary of a finished second with excess, if `now` is past it, merged with one owed
    /// from a failed write.
    pub(crate) fn roll(&mut self, now: u64) -> Option<(u64, u64)> {
        let second = now / 1000;
        let finished = if second == self.second {
            None
        } else {
            let finished = (self.excess > 0).then_some((self.second * 1000, self.excess));
            *self = Self {
                second,
                written: 0,
                excess: 0,
                owed: self.owed,
            };
            finished
        };
        match (self.owed.take(), finished) {
            (Some((a_ts, a)), Some((b_ts, b))) => Some((a_ts.min(b_ts), a + b)),
            (owed, finished) => owed.or(finished),
        }
    }

    /// Keeps a summary whose audit write failed, to hand out again.
    pub(crate) fn owe(&mut self, ts: u64, count: u64) {
        self.owed = match self.owed {
            Some((owed_ts, owed)) => Some((owed_ts.min(ts), owed + count)),
            None => Some((ts, count)),
        };
    }
}

/// Maps a stored actor to the wire string.
pub(crate) fn actor_str(actor: Actor) -> String {
    actor.as_str().to_owned()
}

#[cfg(test)]
mod tests;
