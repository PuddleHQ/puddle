// SPDX-License-Identifier: AGPL-3.0-or-later
//! The audit record union on the wire (docs/spec/rules.md §5).

use puddle_store as store;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{
    AuditPending, AuditRequestRule, AuditRule, AuditRuleSet, ConnectionDecision, ConnectionOrigin,
    ConnectionOutcome, PendingExpiryReason, RuleDeleteReason, SystemReason, WorkspaceOpKind,
    WorkspaceOpOutcome,
};

/// One audit record (rules spec R-24): a tagged union on `type`. `host`, `path` and the like come
/// from the guest, so escape them when rendering. Records written by an older Puddle lack the
/// fields added since, which read as `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuditRecord {
    /// A connection the proxy handled. `host`, `port` and `decision` are `null` on a
    /// `suppressed` summary, which carries `count`.
    Connection {
        /// Epoch ms.
        ts: u64,
        /// The workspace; `null` for Puddle's own connections (`origin` is `puddle`).
        #[schema(required = true)]
        workspace_id: Option<String>,
        /// Whose connection it is. Records written before it existed read as `workspace`.
        #[serde(default)]
        #[schema(required = true)]
        origin: ConnectionOrigin,
        /// The requested host.
        #[schema(required = true)]
        host: Option<String>,
        /// The requested port.
        #[schema(required = true)]
        port: Option<u16>,
        /// The address connected to.
        #[schema(required = true)]
        resolved_ip: Option<String>,
        /// The company-proxy hop that carried it: `DIRECT` or `PROXY host:port`; `null` when no
        /// upstream route is configured.
        #[schema(required = true)]
        upstream: Option<String>,
        /// What happened.
        #[schema(required = true)]
        decision: Option<ConnectionDecision>,
        /// How reaching the destination went after an `allow`: `connected`, `upstream_refused`,
        /// `connect_failed` or `timeout`. `null` where nothing was attempted (a refusal by rule,
        /// guard or limit) and in records written before it existed.
        #[serde(default)]
        #[schema(required = true)]
        outcome: Option<ConnectionOutcome>,
        /// Why: `rule`, `no_rule`, or a block reason.
        reason: String,
        /// The deciding rule.
        #[schema(required = true)]
        rule_id: Option<i64>,
        /// The rule on method and path that refused the request, when `reason` is
        /// `request_rule`; `method` and `path` are that request's. For a tunnel refused with
        /// `request_rule_port`, the lowest-numbered rule on the host. Numbered apart from `rule_id`.
        /// Records written before it existed read as `null`.
        #[serde(default)]
        #[schema(required = true)]
        request_rule: Option<i64>,
        /// The rule set whose entry decided (`system`, `builtin:<slug>`, `user:<id>`).
        #[serde(default)]
        #[schema(required = true)]
        rule_set: Option<String>,
        /// The first address of the answer that an IP rule denied, else the first the address
        /// guard refused, while the connection went to the allowed addresses only. `null` when none was refused and in records written
        /// before it existed.
        #[serde(default)]
        #[schema(required = true)]
        refused_address: Option<String>,
        /// Why `refused_address` was refused: a reason code such as `ip_rule` or `toggle:private`.
        #[serde(default)]
        #[schema(required = true)]
        refused_reason: Option<String>,
        /// `company_network` when the destination is in the company-network category, allowed or
        /// refused; `null` otherwise and in records written before it existed.
        #[serde(default)]
        #[schema(required = true)]
        category: Option<String>,
        /// The signal that put the destination in the company network: `pac_direct`,
        /// `dns_suffix`, `private_access_cgnat` or `vpn_route`; `null` with `category`.
        #[serde(default)]
        #[schema(required = true)]
        signal: Option<String>,
        /// The pending request.
        #[schema(required = true)]
        pending_id: Option<i64>,
        /// The credential binding, by id.
        #[schema(required = true)]
        binding_id: Option<String>,
        /// Whether a credential was injected.
        injected: bool,
        /// The ids (`stand-in:<secret|login>:<name>`) of the secret stand-ins that went out
        /// unchanged, to a host outside their hosts or in the path or query. Records written
        /// before it existed read as empty.
        #[serde(default)]
        #[schema(required = true)]
        unswapped_stand_ins: Vec<String>,
        /// The ids of the stand-ins whose real values an answer from one of their hosts held, put back as
        /// the stand-in before the workspace saw it (`stand-in:secret:<NAME>` or `stand-in:login:<name>`),
        /// each once, at most 16. Records written
        /// before it existed read as empty.
        #[serde(default)]
        #[schema(required = true)]
        scrubbed_stand_ins: Vec<String>,
        /// The kinds of credential the requests carried that Puddle did not add or swap, each
        /// once: `stale_stand_in`, `github_token`, `npm_token`, `gitlab_token`,
        /// `azure_devops_token` or `git_authorization`. Never a value. Records written before it
        /// existed, and connections of a workspace that turned the warning off, read as empty.
        #[serde(default)]
        #[schema(required = true)]
        foreign_credentials: Vec<String>,
        /// The kinds of token the answers held that Puddle does not hold, each once:
        /// `github_token`, `github_fine_grained_token`, `gitlab_token`, `npm_token`,
        /// `azure_devops_token`, `anthropic_key`, `openai_key` or `jwt_with_refresh_token`. Never
        /// a value. Records written before it existed, and connections of a workspace that turned
        /// the warning off, read as empty.
        #[serde(default)]
        #[schema(required = true)]
        answer_credentials: Vec<String>,
        /// HTTP method, where the proxy saw the request in clear.
        #[schema(required = true)]
        method: Option<String>,
        /// HTTP path without query string, where `method` is set.
        #[schema(required = true)]
        path: Option<String>,
        /// Whether `path` was cut to fit.
        path_truncated: bool,
        /// Bytes from the guest.
        bytes_up: u64,
        /// Bytes to the guest.
        bytes_down: u64,
        /// Records summarised, on a `suppressed` summary.
        #[schema(required = true)]
        count: Option<u64>,
    },
    /// A new pending request.
    PendingCreated {
        /// Epoch ms.
        ts: u64,
        /// The request.
        pending: AuditPending,
    },
    /// A pending request approved or denied.
    PendingDecided {
        /// Epoch ms.
        ts: u64,
        /// The request as decided.
        pending: AuditPending,
    },
    /// A pending request expired.
    PendingExpired {
        /// Epoch ms.
        ts: u64,
        /// The request as expired.
        pending: AuditPending,
        /// Why.
        reason: PendingExpiryReason,
    },
    /// Requests an earlier version held back over a workspace's limit. Only in old records.
    PendingSuppressed {
        /// Epoch ms.
        ts: u64,
        /// The workspace.
        workspace_id: String,
        /// Requests held back since the previous record.
        count: u64,
    },
    /// A rule created.
    RuleCreated {
        /// Epoch ms.
        ts: u64,
        /// The rule.
        rule: AuditRule,
    },
    /// A rule changed.
    RuleUpdated {
        /// Epoch ms.
        ts: u64,
        /// The rule before.
        before: AuditRule,
        /// The rule after.
        rule: AuditRule,
        /// Who changed it.
        actor: String,
    },
    /// A rule deleted.
    RuleDeleted {
        /// Epoch ms.
        ts: u64,
        /// The rule as it was.
        rule: AuditRule,
        /// Why.
        reason: RuleDeleteReason,
        /// Who deleted it.
        actor: String,
    },
    /// A rule removed after it expired.
    RuleExpired {
        /// Epoch ms.
        ts: u64,
        /// The rule.
        rule: AuditRule,
    },
    /// A rule on method and path created.
    RequestRuleCreated {
        /// Epoch ms.
        ts: u64,
        /// The rule.
        rule: AuditRequestRule,
    },
    /// A rule on method and path deleted.
    RequestRuleDeleted {
        /// Epoch ms.
        ts: u64,
        /// The rule as it was.
        rule: AuditRequestRule,
        /// Why.
        reason: RuleDeleteReason,
        /// Who deleted it.
        actor: String,
    },
    /// A rule set made.
    RuleSetCreated {
        /// Epoch ms.
        ts: u64,
        /// The set.
        rule_set: AuditRuleSet,
        /// Who made it.
        actor: String,
    },
    /// A rule set renamed or described anew.
    RuleSetUpdated {
        /// Epoch ms.
        ts: u64,
        /// The set before.
        before: AuditRuleSet,
        /// The set after.
        rule_set: AuditRuleSet,
        /// Who changed it.
        actor: String,
    },
    /// A rule set deleted with its entries (each also has a `rule_deleted`).
    RuleSetDeleted {
        /// Epoch ms.
        ts: u64,
        /// The set as it was.
        rule_set: AuditRuleSet,
        /// Who deleted it.
        actor: String,
    },
    /// A rule set switched on or off, or back to following the next level.
    RuleSetSwitched {
        /// Epoch ms.
        ts: u64,
        /// `builtin:<slug>` or `user:<id>`.
        set_id: String,
        /// The workspace whose override changed; `null` for every workspace.
        #[schema(required = true)]
        workspace_id: Option<String>,
        /// On, off, or `null` to follow the next level.
        #[schema(required = true)]
        enabled: Option<bool>,
        /// Who switched it.
        actor: String,
    },
    /// A Puddle update changed a built-in set's entries.
    RuleSetChanged {
        /// Epoch ms.
        ts: u64,
        /// `builtin:<slug>`.
        set_id: String,
        /// Patterns added.
        added: Vec<String>,
        /// Patterns removed.
        removed: Vec<String>,
    },
    /// The reasons for the System managed hosts changed, because your setup did.
    SystemManagedChanged {
        /// Epoch ms.
        ts: u64,
        /// The workspace; `null` for every workspace.
        #[schema(required = true)]
        workspace_id: Option<String>,
        /// Reasons that now apply.
        added: Vec<SystemReason>,
        /// Reasons that no longer apply.
        removed: Vec<SystemReason>,
    },
    /// A workspace operation, or a change to the identities and repository lists a workspace
    /// uses: who asked, which workspace and how it ended.
    WorkspaceOp {
        /// Epoch ms.
        ts: u64,
        /// What was done.
        op: WorkspaceOpKind,
        /// The workspace; `null` for a change that belongs to none (an identity).
        #[schema(required = true)]
        workspace_id: Option<String>,
        /// How it ended.
        outcome: WorkspaceOpOutcome,
        /// Who asked: `cli`, `ui`, `api` or `system`.
        actor: String,
        /// Why it failed, or what the operation touched; `null` when there is nothing to add.
        #[schema(required = true)]
        detail: Option<String>,
    },
    /// The oldest records were deleted to keep the audit under its size cap.
    AuditTrimmed {
        /// Epoch ms.
        ts: u64,
        /// Records deleted.
        deleted_records: u64,
        /// `ts` of the oldest record left; `null` if none.
        #[schema(required = true)]
        oldest_ts_kept: Option<u64>,
    },
}

impl From<store::ConnectionRecord> for AuditRecord {
    fn from(record: store::ConnectionRecord) -> Self {
        let store::ConnectionRecord {
            ts,
            workspace_id,
            origin,
            host,
            port,
            resolved_ip,
            upstream,
            decision,
            outcome,
            reason,
            rule_id,
            request_rule,
            rule_set,
            refused_address,
            refused_reason,
            category,
            signal,
            pending_id,
            binding_id,
            injected,
            unswapped_stand_ins,
            scrubbed_stand_ins,
            foreign_credentials,
            answer_credentials,
            method,
            path,
            path_truncated,
            bytes_up,
            bytes_down,
            count,
        } = record;
        Self::Connection {
            ts,
            workspace_id,
            host,
            port,
            resolved_ip,
            upstream,
            origin: origin.into(),
            decision: decision.map(Into::into),
            outcome: outcome.map(Into::into),
            reason,
            rule_id,
            request_rule,
            rule_set,
            refused_address,
            refused_reason,
            category,
            signal,
            pending_id,
            binding_id,
            injected,
            unswapped_stand_ins,
            scrubbed_stand_ins,
            foreign_credentials,
            answer_credentials,
            method,
            path,
            path_truncated,
            bytes_up,
            bytes_down,
            count,
        }
    }
}

impl From<store::AuditRecord> for AuditRecord {
    #[expect(
        clippy::too_many_lines,
        reason = "one plain arm per record type; splitting it would hide a missing one"
    )]
    fn from(record: store::AuditRecord) -> Self {
        use store::AuditRecord as R;
        match record {
            R::Connection(record) => record.into(),
            R::PendingCreated { ts, pending } => Self::PendingCreated {
                ts,
                pending: pending.into(),
            },
            R::PendingDecided { ts, pending } => Self::PendingDecided {
                ts,
                pending: pending.into(),
            },
            R::PendingExpired {
                ts,
                pending,
                reason,
            } => Self::PendingExpired {
                ts,
                pending: pending.into(),
                reason: match reason {
                    store::PendingExpiryReason::Stale => PendingExpiryReason::Stale,
                    store::PendingExpiryReason::WorkspaceDeleted => {
                        PendingExpiryReason::WorkspaceDeleted
                    }
                },
            },
            R::PendingSuppressed {
                ts,
                workspace_id,
                count,
            } => Self::PendingSuppressed {
                ts,
                workspace_id,
                count,
            },
            R::RuleCreated { ts, rule } => Self::RuleCreated {
                ts,
                rule: rule.into(),
            },
            R::RuleUpdated {
                ts,
                before,
                rule,
                actor,
            } => Self::RuleUpdated {
                ts,
                before: before.into(),
                rule: rule.into(),
                actor,
            },
            R::RuleDeleted {
                ts,
                rule,
                reason,
                actor,
            } => Self::RuleDeleted {
                ts,
                rule: rule.into(),
                reason: reason.into(),
                actor,
            },
            R::RuleExpired { ts, rule } => Self::RuleExpired {
                ts,
                rule: rule.into(),
            },
            R::RequestRuleCreated { ts, rule } => Self::RequestRuleCreated {
                ts,
                rule: rule.into(),
            },
            R::RequestRuleDeleted {
                ts,
                rule,
                reason,
                actor,
            } => Self::RequestRuleDeleted {
                ts,
                rule: rule.into(),
                reason: reason.into(),
                actor,
            },
            R::RuleSetCreated {
                ts,
                rule_set,
                actor,
            } => Self::RuleSetCreated {
                ts,
                rule_set: rule_set.into(),
                actor,
            },
            R::RuleSetUpdated {
                ts,
                before,
                rule_set,
                actor,
            } => Self::RuleSetUpdated {
                ts,
                before: before.into(),
                rule_set: rule_set.into(),
                actor,
            },
            R::RuleSetDeleted {
                ts,
                rule_set,
                actor,
            } => Self::RuleSetDeleted {
                ts,
                rule_set: rule_set.into(),
                actor,
            },
            R::RuleSetSwitched {
                ts,
                set_id,
                workspace_id,
                enabled,
                actor,
            } => Self::RuleSetSwitched {
                ts,
                set_id,
                workspace_id,
                enabled,
                actor,
            },
            R::RuleSetChanged {
                ts,
                set_id,
                added,
                removed,
            } => Self::RuleSetChanged {
                ts,
                set_id,
                added,
                removed,
            },
            R::SystemManagedChanged {
                ts,
                workspace_id,
                added,
                removed,
            } => Self::SystemManagedChanged {
                ts,
                workspace_id,
                added: SystemReason::parse_all(&added),
                removed: SystemReason::parse_all(&removed),
            },
            R::WorkspaceOp {
                ts,
                op,
                workspace_id,
                outcome,
                actor,
                detail,
            } => Self::WorkspaceOp {
                ts,
                op: op.into(),
                workspace_id,
                outcome: outcome.into(),
                actor,
                detail,
            },
            R::AuditTrimmed {
                ts,
                deleted_records,
                oldest_ts_kept,
            } => Self::AuditTrimmed {
                ts,
                deleted_records,
                oldest_ts_kept,
            },
        }
    }
}
