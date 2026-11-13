// SPDX-License-Identifier: GPL-3.0-or-later
//! Rules and pending requests on the wire (docs/spec/rules.md).

use std::time::Duration;

use puddle_store as store;
use puddle_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::ApiError;

// ---------------------------------------------------------------------------------------------
// Rules and pending requests (docs/spec/rules.md)

/// Allow or deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Let the connection through.
    Allow,
    /// Refuse it.
    Deny,
}

impl From<store::Effect> for Effect {
    fn from(effect: store::Effect) -> Self {
        match effect {
            store::Effect::Allow => Self::Allow,
            store::Effect::Deny => Self::Deny,
        }
    }
}

impl From<Effect> for store::Effect {
    fn from(effect: Effect) -> Self {
        match effect {
            Effect::Allow => Self::Allow,
            Effect::Deny => Self::Deny,
        }
    }
}

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// The `puddle` command line.
    Cli,
    /// The desktop UI.
    Ui,
    /// The HTTP API.
    Api,
    /// Puddle itself (expiry, workspace deletion).
    System,
    /// A workspace create, or adding a repository: it allows the host of the repository.
    WorkspaceCreate,
}

impl Actor {
    pub(crate) fn from_store(actor: store::Actor) -> Result<Self, ApiError> {
        Ok(match actor {
            store::Actor::Cli => Self::Cli,
            store::Actor::Ui => Self::Ui,
            store::Actor::Api => Self::Api,
            store::Actor::System => Self::System,
            store::Actor::WorkspaceCreate => Self::WorkspaceCreate,
            other => {
                return Err(ApiError::internal(&format_args!(
                    "unmapped actor {other:?}"
                )));
            }
        })
    }
}

/// Where a pending request is in its life (R-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendingState {
    /// Waiting for the user.
    Requested,
    /// Approved.
    Allowed,
    /// Denied.
    Denied,
    /// Closed without a decision.
    Expired,
}

impl From<store::PendingState> for PendingState {
    fn from(state: store::PendingState) -> Self {
        match state {
            store::PendingState::Requested => Self::Requested,
            store::PendingState::Allowed => Self::Allowed,
            store::PendingState::Denied => Self::Denied,
            store::PendingState::Expired => Self::Expired,
        }
    }
}

/// A connection that matched no rule and waits for the user (R-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingRequest {
    /// Row id, never reused.
    pub id: i64,
    /// The requesting workspace.
    pub workspace: WorkspaceId,
    /// The requested host: a normalised name (punycode, lower case) or an IP literal. Untrusted
    /// (it comes from the guest): escape it when rendering.
    pub host: String,
    /// The requested port.
    pub port: u16,
    /// Epoch ms of the first request.
    pub first_seen: u64,
    /// Epoch ms of the latest request.
    pub last_seen: u64,
    /// How many requests the row stands for.
    pub attempts: u64,
    /// Where the row is in its life.
    pub state: PendingState,
    /// Epoch ms it was decided or expired.
    #[schema(required = true)]
    pub decided_at: Option<u64>,
    /// Who closed it.
    #[schema(required = true)]
    pub decided_by: Option<Actor>,
    /// The rule that decided it.
    #[schema(required = true)]
    pub rule_id: Option<i64>,
    /// The rule set whose entry decided it (`system`, `builtin:<slug>`, `user:<id>`), when one
    /// did.
    #[schema(required = true)]
    pub rule_set: Option<String>,
    /// For an open request to a local destination (host loopback, private network, link-local,
    /// metadata, special) whose toggle is off in this workspace: the toggle that blocks approval.
    /// An allow rule would not let the connection through until the toggle is on. `null`
    /// otherwise, and always for a closed request.
    #[schema(required = true)]
    pub blocked_by: Option<puddle_types::LocalCategory>,
    /// For an open request to an IP literal on the company network, whether or not its toggle is
    /// on: what put the address there (`pac_direct`, `dns_suffix`, `private_access_cgnat` or
    /// `vpn_route`). `null` otherwise. A client that reads the address ranges itself needs it to
    /// tell a company address from the plain range it also falls in.
    #[schema(required = true)]
    pub company_signal: Option<String>,
}

impl PendingRequest {
    pub(crate) fn from_store(row: store::PendingRow) -> Result<Self, ApiError> {
        let store::PendingRow {
            id,
            workspace,
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
        Ok(Self {
            id: id.0,
            workspace,
            host: host.to_string(),
            port,
            first_seen,
            last_seen,
            attempts,
            state: state.into(),
            decided_at,
            decided_by: decided_by.map(Actor::from_store).transpose()?,
            rule_id: rule_id.map(|r| r.0),
            rule_set,
            blocked_by: None,
            company_signal: None,
        })
    }
}

/// Pending requests, most recent first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PendingList {
    /// The requests.
    pub requests: Vec<PendingRequest>,
}

/// Open requests that share a registrable domain (R-18). Grouping is for display only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InboxGroup {
    /// `example.co.uk`, or the IP literal.
    pub registrable_domain: String,
    /// Open requests, most recent first.
    pub requests: Vec<PendingRequest>,
}

/// The inbox: open requests grouped by registrable domain, most recent group first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Inbox {
    /// The groups.
    pub groups: Vec<InboxGroup>,
    /// The most open requests Puddle lists for one workspace. A workspace with this many has a
    /// full list: a new host it asks about is answered as waiting but gets no row until the user
    /// answers some (rules spec R-45).
    pub open_limit: u64,
}

/// Which workspaces a rule applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RuleScope {
    /// Every workspace.
    Global,
    /// One workspace.
    Workspace {
        /// The workspace.
        workspace: WorkspaceId,
    },
    /// An entry of a rule set you made: it applies wherever the set is on, and your own global
    /// and workspace rules decide first.
    Set {
        /// The set's number (its id is `user:<set>`).
        set: i64,
    },
}

impl RuleScope {
    pub(crate) fn from_store(scope: store::Scope) -> Result<Self, ApiError> {
        Ok(match scope {
            store::Scope::Global => Self::Global,
            store::Scope::Workspace(workspace) => Self::Workspace { workspace },
            store::Scope::Set(set) => Self::Set { set },
            other => {
                return Err(ApiError::internal(&format_args!(
                    "unmapped scope {other:?}"
                )));
            }
        })
    }
}

impl From<RuleScope> for store::Scope {
    fn from(scope: RuleScope) -> Self {
        match scope {
            RuleScope::Global => Self::Global,
            RuleScope::Workspace { workspace } => Self::Workspace(workspace),
            RuleScope::Set { set } => Self::Set(set),
        }
    }
}

/// How a rule's pattern matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PatternKind {
    /// The identical host.
    Exact,
    /// The host and every name under it (`.example.com`).
    Suffix,
}

/// A rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Rule {
    /// Row id, never reused.
    pub id: i64,
    /// Global or one workspace.
    pub scope: RuleScope,
    /// `example.com` (exact) or `.example.com` (suffix).
    pub pattern: String,
    /// Exact or suffix.
    pub pattern_kind: PatternKind,
    /// Allow or deny.
    pub effect: Effect,
    /// Epoch ms from which the rule no longer matches; `null` is permanent.
    #[schema(required = true)]
    pub expires_at: Option<u64>,
    /// Epoch ms.
    pub created_at: u64,
    /// Who created it.
    pub created_by: Actor,
    /// The pending request it was decided from.
    #[schema(required = true)]
    pub source_pending_id: Option<i64>,
}

impl Rule {
    pub(crate) fn from_store(rule: store::Rule) -> Result<Self, ApiError> {
        let store::Rule {
            id,
            user: _,
            scope,
            pattern,
            effect,
            expires_at,
            created_at,
            created_by,
            source_pending_id,
        } = rule;
        Ok(Self {
            id: id.0,
            scope: RuleScope::from_store(scope)?,
            pattern_kind: match pattern.kind() {
                puddle_types::PatternKind::Exact => PatternKind::Exact,
                puddle_types::PatternKind::Suffix => PatternKind::Suffix,
            },
            pattern: pattern.to_string(),
            effect: effect.into(),
            expires_at,
            created_at,
            created_by: Actor::from_store(created_by)?,
            source_pending_id: source_pending_id.map(|p| p.0),
        })
    }
}

/// Every rule, expired ones the sweeper hasn't removed yet included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct RuleList {
    /// The rules.
    pub rules: Vec<Rule>,
}

/// Whether an approval or denial covers only the request's workspace or every workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeChoice {
    /// Only the request's workspace (the default).
    #[default]
    Workspace,
    /// Every workspace.
    Global,
}

/// The choices of an approve or deny (R-15). Each one left out stays at its narrowest default:
/// this workspace, the exact host, permanent.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    /// This workspace (default) or every workspace.
    #[serde(default)]
    pub scope: ScopeChoice,
    /// Put the rule into this rule set of yours (the number of `user:<id>`) instead: it then
    /// applies wherever the set is on. `scope` must be left at `workspace`. The set must be on
    /// for the request's workspace.
    #[serde(default)]
    pub rule_set: Option<i64>,
    /// A suffix of the host (`example.com`, `.example.com` or `*.example.com`) to cover every
    /// name under it; left out or `null` means the exact host. A public suffix is refused.
    #[serde(default)]
    pub suffix: Option<String>,
    /// Seconds until the rule expires; left out or `null` is permanent.
    #[serde(default)]
    #[schema(minimum = 1)]
    pub expires_in_secs: Option<u64>,
}

impl DecisionRequest {
    pub(crate) fn into_resolution(self, effect: Effect) -> Result<store::Resolution, ApiError> {
        let Self {
            scope,
            rule_set,
            suffix,
            expires_in_secs,
        } = self;
        let mut resolution = match effect {
            Effect::Allow => store::Resolution::allow(),
            Effect::Deny => store::Resolution::deny(),
        };
        resolution.scope = match (scope, rule_set) {
            (ScopeChoice::Workspace, None) => store::ScopeChoice::Workspace,
            (ScopeChoice::Global, None) => store::ScopeChoice::Global,
            (ScopeChoice::Workspace, Some(set)) => store::ScopeChoice::Set(set),
            (ScopeChoice::Global, Some(_)) => {
                return Err(ApiError::invalid(
                    "rule_set: a rule set's entry applies wherever the set is on; leave scope out",
                ));
            }
        };
        resolution.pattern = match suffix {
            None => store::PatternChoice::Exact,
            Some(text) => {
                let (_, base) = normalise_pattern("suffix", &text)?;
                store::PatternChoice::Suffix(base)
            }
        };
        resolution.expires_in = expires_in_secs.map(Duration::from_secs);
        Ok(resolution)
    }
}

/// What an approve or deny did (R-16, R-17).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DecisionOutcome {
    /// The request, now decided.
    pub request: PendingRequest,
    /// The rule created.
    pub rule: Rule,
    /// Other open requests the new rule closed the same way.
    pub also_closed: Vec<i64>,
}

impl DecisionOutcome {
    pub(crate) fn from_store(decided: store::Decided) -> Result<Self, ApiError> {
        let store::Decided {
            row,
            rule,
            also_closed,
        } = decided;
        Ok(Self {
            request: PendingRequest::from_store(row)?,
            rule: Rule::from_store(rule)?,
            also_closed: also_closed.into_iter().map(|p| p.0).collect(),
        })
    }
}

/// A rule to create directly (not from a pending request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewRuleRequest {
    /// Global or one workspace.
    pub scope: RuleScope,
    /// `example.com` (exact), or `.example.com` / `*.example.com` (suffix; a public suffix is
    /// refused).
    pub pattern: String,
    /// Allow or deny.
    pub effect: Effect,
    /// Epoch ms from which the rule no longer matches; left out or `null` is permanent.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

impl NewRuleRequest {
    pub(crate) fn into_new_rule(self) -> Result<store::NewRule, ApiError> {
        let Self {
            scope,
            pattern,
            effect,
            expires_at,
        } = self;
        let (suffix, base) = normalise_pattern("pattern", &pattern)?;
        let text = if suffix { format!(".{base}") } else { base };
        Ok(store::NewRule {
            scope: scope.into(),
            pattern: store::Pattern::parse(&text)
                .map_err(|err| ApiError::invalid(format!("pattern: {err}")))?,
            effect: effect.into(),
            expires_at,
            created_by: store::Actor::Api,
        })
    }
}

/// Normalises what a person typed as a rule pattern or suffix, with the proxy's own normaliser, so
/// a rule matches exactly what the proxy will look up. Returns whether it was written as a suffix
/// (`.example.com` or `*.example.com`) and the normalised host: lower case, Unicode to punycode,
/// one trailing dot dropped, IPv6 in its canonical form. Surrounding whitespace is ignored.
///
/// URLs, ports and anything else that is not a host are refused with a message that starts with
/// `field`, so a client can show it next to that field.
pub(super) fn normalise_pattern(field: &str, input: &str) -> Result<(bool, String), ApiError> {
    let text = input.trim();
    let (suffix, rest) = match text.strip_prefix("*.").or_else(|| text.strip_prefix('.')) {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let refuse = |reason: &str| Err(ApiError::invalid(format!("{field}: {reason}")));
    if rest.contains('/') || rest.contains('@') || rest.contains('?') || rest.contains('#') {
        return refuse("give a host name, not a URL");
    }
    if let Some((host, port)) = rest.rsplit_once(':')
        && !host.contains(':')
        && !host.is_empty()
        && !port.is_empty()
        && port.bytes().all(|b| b.is_ascii_digit())
    {
        return refuse("a rule covers every port, leave the port out");
    }
    let target = puddle_netpolicy::normalise_host(rest)
        .map_err(|err| ApiError::invalid(format!("{field}: {err}")))?;
    Ok((suffix, target.host().to_string()))
}

/// A rule's new expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RuleExpiryRequest {
    /// Epoch ms in the future, or `null` to make the rule permanent.
    #[schema(required = true)]
    pub expires_at: Option<u64>,
}
