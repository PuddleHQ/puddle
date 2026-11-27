// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `connection` audit record (R-24) and how a proxy event becomes one.

use puddle_types::{
    ConnectionDecision, ConnectionEvent, ConnectionOrigin, ConnectionOutcome, ConnectionProtocol,
    ConnectionReason, GitOperation, LocalCategory, WorkspaceId,
};
use serde::{Deserialize, Serialize};

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
    /// What `method` and `path` are: `http` or `ssh` (Git over SSH, where `method` is the
    /// command). `null` with no `method`; absent in records written before it existed.
    #[serde(default)]
    pub protocol: Option<ConnectionProtocol>,
    /// HTTP method: plain-HTTP requests, tunnels that carry plain HTTP, terminated hosts; for
    /// `protocol: ssh` the Git command.
    pub method: Option<String>,
    /// HTTP path without query string, where `method` is set.
    pub path: Option<String>,
    /// `push` or `fetch` for a Git smart-HTTP request (`service=git-receive-pack` or
    /// `git-upload-pack` in the query, which `path` drops). Absent in records written before it
    /// existed.
    #[serde(default)]
    pub git_operation: Option<GitOperation>,
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
        let (protocol, method, path, git_operation) =
            event
                .http
                .as_ref()
                .map_or((None, None, None, None), |http| {
                    (
                        Some(http.protocol()),
                        Some(http.method().to_owned()),
                        Some(http.path().to_owned()),
                        http.git_operation(),
                    )
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
            protocol,
            method,
            path,
            git_operation,
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
            protocol: None,
            method: None,
            path: None,
            git_operation: None,
            path_truncated: false,
            bytes_up: 0,
            bytes_down: 0,
            count: Some(count),
        }
    }
}
