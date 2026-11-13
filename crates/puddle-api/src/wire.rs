// SPDX-License-Identifier: GPL-3.0-or-later
//! The API's wire types: every request and response body, as published in the `OpenAPI` spec
//! (ADR 0002 conventions, ADR 0004 additions).
//!
//! - `snake_case` fields; unit enums as `snake_case` strings; tagged enums use `type` (or
//!   `state` for consents), never untagged.
//! - Responses always carry every field; a missing value is `null`, never an absent key.
//! - Requests (`*Request`) refuse unknown fields; optional request fields may be left out.
//! - Timestamps are epoch milliseconds; row ids are integers; names are strings.
//! - No `#[serde(flatten)]`: storage documents (which keep unknown fields that way) are mapped
//!   to these types field by field.
//!
//! Domain types (`puddle-store`, `puddle-settings`) are mapped here rather than exposed, so a
//! storage change can't silently change the contract.

use std::time::Duration;

use puddle_settings as settings;
use puddle_store as store;
use puddle_types::{CpuCount, MemoryMib, WorkspaceId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::ApiError;

mod app_icon;
mod auth;
mod credentials;
mod doctor;
mod environment;
mod first_run;
mod identities;
mod interception;
mod logins;
mod network_health;
mod orphans;
mod ports;
mod proxy_sign_in;
mod repos;
mod repositories;
mod repository_hosts;
mod request_rules;
mod rule_sets;
mod shell_init;
mod ssh_agent;
mod start_folder;
mod workspaces;

pub use app_icon::{AppIconChoice, AppIcons, BuiltInIcon, CustomIcon};
pub use auth::{
    AuthStatus, ChangePasswordRequest, DeviceList, DeviceView, InviteRequest,
    PasswordSignInRequest, SessionGrant, SetupRequest, SignInMethodKind, SignInMethodRequest,
    SignInMethodView, SignInMethods, UserView,
};
pub(crate) use credentials::source_problem;
pub use credentials::{
    CheckRequest, CheckResult, FoundAccount, FoundAccounts, FoundProblem, FoundVia, SignInRequest,
    SignInStarted, StoreTokenRequest, StoredToken,
};
pub use doctor::{DoctorCheck, DoctorReport, DoctorStatus};
pub use environment::{
    EnvKind, EnvList, EnvScope, EnvSetRequest, EnvVariable, SecretPlace, SecretText, WorkspaceEnv,
};
pub use first_run::{DevCertTrustResult, DevCertificate, FirstRun, FirstRunRequest};
pub use identities::{
    AttachIdentityRequest, CheckedBy, CredentialCoverage, CredentialSource, GitDefaultsView,
    GitRepoRequest, GitRepoToggles, GitRepoView, GitSwitchesRequest, IdentityAuthor,
    IdentityCredential, IdentityCredentialView, IdentityDeleted, IdentityList,
    IdentityOrderRequest, IdentityRequest, IdentitySigning, IdentityView, LastCheck,
    NewWorkspaceIdentity, NewWorkspaceIdentityBasis, SwitchSource, TokenLeft, WorkspaceGitView,
    WorkspaceIdentitiesRequest, WorkspaceSwitchesRequest,
};
pub use interception::{
    GlobalNeverDecrypt, InterceptionRequest, InterceptionSettingsRequest, InterceptionSettingsView,
    InterceptionView, LearnedHost, LearnedHosts, LearnedLimits, MakePermanentRequest,
    PermanentScope, WorkspaceMode,
};
pub use logins::{KeptLoginList, KeptLoginView};
pub use network_health::{
    CompanyNetworkReport, DeadProxy, NetworkHealth, PacState, ProxyDetected, ProxyMode,
    ProxyProblem, ProxyProblemKind, ProxyReport, PullProxyReport, RootKind, RootsReport,
    RouteDecision, RouteSource, SignInAttempt, SignInReport, SignInResult, SkippedRoot, SyncedRoot,
};
pub use orphans::{OrphanVolumeList, OrphanVolumeView, RestoreOrphanRequest};
pub use ports::{PortEntry, PortForwardRequest, WorkspacePorts};
pub use proxy_sign_in::{ProxySignInList, ProxySignInRequest, StoredSignIn};
pub use repos::{
    AccountProfile, AccountProfileRequest, RepoListState, RepoListing, RepoNote, RepoNoteCode,
    RepoProblem, RepoProblemCode, RepoRefreshRequest, RepoRole, RepoSource, RepoSources, RepoView,
    RepoVisibility,
};
pub use repositories::{AddRepositoryRequest, Checkout, CheckoutManage, Checkouts, RemoteNote};
pub use repository_hosts::{
    AllowedHost, RepositoryHost, RepositoryHostState, RepositoryHosts, RepositoryHostsRequest,
};
pub use request_rules::{NewRequestRuleRequest, RequestRule, RequestRuleList};
pub use rule_sets::{
    NewRuleSetRequest, RuleSetEntry, RuleSetKind, RuleSetList, RuleSetOverride,
    RuleSetSwitchRequest, RuleSetSwitched, RuleSetUpdateRequest, RuleSetView, SystemManagedHost,
    SystemReason,
};
pub use shell_init::{ShellInitRequest, ShellInitSaved, ShellInitView};
pub use ssh_agent::{SshAgentHostRequest, SshAgentHostView, SshAgentView};
pub use start_folder::{StartFolderRequest, StartFolderSet};

pub use workspaces::{
    AttachMode, AttachRequest, AttachResponse, DeleteCheck, DeleteWorkspaceRequest, FindingList,
    GrowRootDiskRequest, NewRepositoryRequest, NewWorkspaceRequest, RenameWorkspaceRequest,
    RepoFindings, SessionUserRequest, Workspace, WorkspaceList, WorkspaceLookupConflict,
    WorkspaceMatch, WorkspaceOperation, WorkspaceRepository,
};

// ---------------------------------------------------------------------------------------------
// Service

/// The API's identity, for a client to check it reached Puddle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Health {
    /// Puddle's version.
    pub version: String,
    /// The API contract's version ([`crate::API_VERSION`]).
    pub api_version: String,
    /// The build of the app this server serves, read from the build's own `_app/version.json`: a
    /// page that carries another build is stale and reloads. `null` when the server serves no
    /// app (a development server behind the Vite proxy).
    #[schema(required = true)]
    pub ui_build_id: Option<String>,
}

/// The background problems that stand.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ProblemList {
    /// Ordered by key.
    pub problems: Vec<puddle_types::Problem>,
}

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
pub(crate) fn normalise_pattern(field: &str, input: &str) -> Result<(bool, String), ApiError> {
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

// ---------------------------------------------------------------------------------------------
// Settings (puddle-settings)

/// The settings every workspace has. On a global document they are the defaults for every
/// workspace; on a workspace they override those. `null` means "not set here": the next level (the
/// global value, then Puddle's default) applies.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingsLayer {
    /// Guest memory in MiB (256 to 1 048 576); applies at the next start.
    #[serde(default)]
    #[schema(required = true, minimum = 256, maximum = 1_048_576)]
    pub memory: Option<u32>,
    /// Guest vCPUs (1 to 32); applies at the next start.
    #[serde(default)]
    #[schema(required = true, minimum = 1, maximum = 32)]
    pub cpus: Option<u8>,
    /// Which local destination categories may be approved (all off by default).
    #[serde(default)]
    #[schema(required = true)]
    pub local_toggles: LocalToggles,
    /// Whether a suffix rule may reach local addresses.
    #[serde(default)]
    #[schema(required = true)]
    pub wildcards_reach_local: Option<bool>,
    /// Seconds a browser VS Code session survives a disconnect (30 to 86 400).
    #[serde(default)]
    #[schema(required = true, minimum = 30, maximum = 86_400)]
    pub reconnection_grace: Option<u32>,
    /// Whether the browser window's zoom hotkeys work.
    #[serde(default)]
    #[schema(required = true)]
    pub zoom_hotkeys: Option<bool>,
    /// What a programmatic clipboard read does.
    #[serde(default)]
    #[schema(required = true)]
    pub clipboard_read: Option<ClipboardRead>,
    /// Whether Puddle opens an SSH way into the workspace for the user's own tools (desktop
    /// VS Code, a terminal `ssh`). Off by default; while off there is no SSH endpoint and no ssh
    /// config entry. Turning it on marks the workspace trusted.
    #[serde(default)]
    #[schema(required = true)]
    pub direct_ssh: Option<bool>,
    /// Whether a login made inside the workspace (Claude Code, `gh`, Copilot CLI) is captured:
    /// Puddle keeps the real token on the host and the workspace holds a stand-in that works
    /// only through Puddle. On by default; applies at the workspace's next start.
    #[serde(default)]
    #[schema(required = true)]
    pub capture_logins: Option<bool>,
    /// Whether Puddle warns when a request carries a credential it did not add (a token the
    /// workspace holds itself). The request goes out either way; this only switches the notice
    /// and the flag in the activity log. On by default.
    #[serde(default)]
    #[schema(required = true)]
    pub warn_foreign_credentials: Option<bool>,
}

impl From<&settings::WorkspaceLayer> for SettingsLayer {
    fn from(layer: &settings::WorkspaceLayer) -> Self {
        Self {
            memory: layer.memory.map(MemoryMib::get),
            cpus: layer.cpus.map(CpuCount::get),
            local_toggles: (&layer.local_toggles).into(),
            wildcards_reach_local: layer.wildcards_reach_local,
            reconnection_grace: layer
                .reconnection_grace
                .map(settings::ReconnectionGrace::secs),
            zoom_hotkeys: layer.zoom_hotkeys,
            clipboard_read: layer.clipboard_read.map(Into::into),
            direct_ssh: layer.direct_ssh,
            capture_logins: layer.capture_logins,
            warn_foreign_credentials: layer.warn_foreign_credentials,
        }
    }
}

impl SettingsLayer {
    /// Writes these values into `layer`, leaving its unknown fields alone.
    pub(crate) fn apply_to(self, layer: &mut settings::WorkspaceLayer) -> Result<(), ApiError> {
        let invalid = |err: puddle_types::ValidationError| ApiError::invalid(err);
        let Self {
            memory,
            cpus,
            local_toggles,
            wildcards_reach_local,
            reconnection_grace,
            zoom_hotkeys,
            clipboard_read,
            direct_ssh,
            capture_logins,
            warn_foreign_credentials,
        } = self;
        layer.memory = memory.map(MemoryMib::new).transpose().map_err(invalid)?;
        layer.cpus = cpus
            .map(|c| CpuCount::new(u32::from(c)))
            .transpose()
            .map_err(invalid)?;
        local_toggles.apply_to(&mut layer.local_toggles);
        layer.wildcards_reach_local = wildcards_reach_local;
        layer.reconnection_grace = reconnection_grace
            .map(settings::ReconnectionGrace::new)
            .transpose()
            .map_err(invalid)?;
        layer.zoom_hotkeys = zoom_hotkeys;
        layer.clipboard_read = clipboard_read.map(Into::into);
        layer.direct_ssh = direct_ssh;
        layer.capture_logins = capture_logins;
        layer.warn_foreign_credentials = warn_foreign_credentials;
        Ok(())
    }
}

/// Local destination categories a workspace may approve. `null` inherits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct LocalToggles {
    /// The host's loopback addresses.
    #[serde(default)]
    #[schema(required = true)]
    pub loopback: Option<bool>,
    /// Private networks (RFC 1918, ULA).
    #[serde(default)]
    #[schema(required = true)]
    pub private: Option<bool>,
    /// Link-local addresses.
    #[serde(default)]
    #[schema(required = true)]
    pub link_local: Option<bool>,
    /// Cloud metadata endpoints.
    #[serde(default)]
    #[schema(required = true)]
    pub metadata: Option<bool>,
    /// Other special-purpose ranges.
    #[serde(default)]
    #[schema(required = true)]
    pub special: Option<bool>,
    /// The company network. Off by default; on only makes those destinations approvable.
    #[serde(default)]
    #[schema(required = true)]
    pub company_network: Option<bool>,
}

impl From<&settings::LocalToggles> for LocalToggles {
    fn from(t: &settings::LocalToggles) -> Self {
        Self {
            loopback: t.loopback,
            private: t.private,
            link_local: t.link_local,
            metadata: t.metadata,
            special: t.special,
            company_network: t.company_network,
        }
    }
}

impl LocalToggles {
    fn apply_to(self, toggles: &mut settings::LocalToggles) {
        let Self {
            loopback,
            private,
            link_local,
            metadata,
            special,
            company_network,
        } = self;
        toggles.loopback = loopback;
        toggles.private = private;
        toggles.link_local = link_local;
        toggles.metadata = metadata;
        toggles.special = special;
        toggles.company_network = company_network;
    }
}

/// What a programmatic clipboard read in the browser window does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardRead {
    /// Ask each time.
    Ask,
    /// Always allow.
    Allow,
    /// Always refuse.
    Deny,
}

impl From<settings::ClipboardRead> for ClipboardRead {
    fn from(c: settings::ClipboardRead) -> Self {
        match c {
            settings::ClipboardRead::Ask => Self::Ask,
            settings::ClipboardRead::Allow => Self::Allow,
            settings::ClipboardRead::Deny => Self::Deny,
        }
    }
}

impl From<ClipboardRead> for settings::ClipboardRead {
    fn from(c: ClipboardRead) -> Self {
        match c {
            ClipboardRead::Ask => Self::Ask,
            ClipboardRead::Allow => Self::Allow,
            ClipboardRead::Deny => Self::Deny,
        }
    }
}

/// Options for the VS Code server in the guest. `null` means Puddle's default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct VsCodeServer {
    /// Which server browser VS Code runs (default `code_server`). `microsoft` is refused
    /// until the user agreed to Microsoft's terms (`PUT /api/consents/vscode_server`).
    #[serde(default)]
    #[schema(required = true)]
    pub server: Option<ServerChoice>,
    /// Whether the server may send Microsoft telemetry (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub telemetry: Option<bool>,
    /// Whether Puddle updates the server (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub auto_update: Option<bool>,
}

/// The global tool switches. Global only: no workspace value can override them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GlobalTools {
    /// Whether Puddle manages tools with mise in workspaces (default on). Off removes the whole
    /// managed layer for every workspace and keeps every stored value.
    #[serde(default)]
    #[schema(required = true)]
    pub mise: Option<bool>,
}

/// One workspace's tool switches. `null` means Puddle's default (on). While the global switch
/// (`GlobalTools::mise`) is off, they are kept but have no effect.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceTools {
    /// Manage this workspace's tools with mise (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub mise: Option<bool>,
    /// Let mise run the repository's own code in this workspace, that is trust its `mise.toml`
    /// (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub repo_code: Option<bool>,
}

impl From<&settings::WorkspaceTools> for WorkspaceTools {
    fn from(t: &settings::WorkspaceTools) -> Self {
        Self {
            mise: t.mise,
            repo_code: t.repo_code,
        }
    }
}

/// Which VS Code server browser VS Code runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerChoice {
    /// The bundled code-server.
    CodeServer,
    /// Microsoft's VS Code server, downloaded after the user's consent.
    Microsoft,
}

impl From<settings::ServerChoice> for ServerChoice {
    fn from(c: settings::ServerChoice) -> Self {
        match c {
            settings::ServerChoice::CodeServer => Self::CodeServer,
            settings::ServerChoice::Microsoft => Self::Microsoft,
        }
    }
}

impl From<ServerChoice> for settings::ServerChoice {
    fn from(c: ServerChoice) -> Self {
        match c {
            ServerChoice::CodeServer => Self::CodeServer,
            ServerChoice::Microsoft => Self::Microsoft,
        }
    }
}

/// The colour theme of Puddle's window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThemeChoice {
    /// Follow the operating system.
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl From<settings::ThemeChoice> for ThemeChoice {
    fn from(c: settings::ThemeChoice) -> Self {
        match c {
            settings::ThemeChoice::System => Self::System,
            settings::ThemeChoice::Light => Self::Light,
            settings::ThemeChoice::Dark => Self::Dark,
        }
    }
}

impl From<ThemeChoice> for settings::ThemeChoice {
    fn from(c: ThemeChoice) -> Self {
        match c {
            ThemeChoice::System => Self::System,
            ThemeChoice::Light => Self::Light,
            ThemeChoice::Dark => Self::Dark,
        }
    }
}

/// How much room the window leaves around its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DensityChoice {
    /// The roomier layout.
    Comfortable,
    /// Tighter spacing, so more fits on screen.
    Compact,
}

impl From<settings::DensityChoice> for DensityChoice {
    fn from(c: settings::DensityChoice) -> Self {
        match c {
            settings::DensityChoice::Comfortable => Self::Comfortable,
            settings::DensityChoice::Compact => Self::Compact,
        }
    }
}

impl From<DensityChoice> for settings::DensityChoice {
    fn from(c: DensityChoice) -> Self {
        match c {
            DensityChoice::Comfortable => Self::Comfortable,
            DensityChoice::Compact => Self::Compact,
        }
    }
}

/// Which colour palette the window uses over the light or dark base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PaletteChoice {
    /// Calm water blues with duck yellow for what needs the user (the default).
    Pond,
    /// Violet blues.
    Dusk,
    /// Soft greens.
    Meadow,
    /// Warm reds and ambers.
    Ember,
    /// Neutral greys with a blue accent.
    Graphite,
    /// The user's own colours, Pond's where one is not set.
    Custom,
}

impl From<settings::PaletteChoice> for PaletteChoice {
    fn from(c: settings::PaletteChoice) -> Self {
        match c {
            settings::PaletteChoice::Pond => Self::Pond,
            settings::PaletteChoice::Dusk => Self::Dusk,
            settings::PaletteChoice::Meadow => Self::Meadow,
            settings::PaletteChoice::Ember => Self::Ember,
            settings::PaletteChoice::Graphite => Self::Graphite,
            settings::PaletteChoice::Custom => Self::Custom,
        }
    }
}

impl From<PaletteChoice> for settings::PaletteChoice {
    fn from(c: PaletteChoice) -> Self {
        match c {
            PaletteChoice::Pond => Self::Pond,
            PaletteChoice::Dusk => Self::Dusk,
            PaletteChoice::Meadow => Self::Meadow,
            PaletteChoice::Ember => Self::Ember,
            PaletteChoice::Graphite => Self::Graphite,
            PaletteChoice::Custom => Self::Custom,
        }
    }
}

/// What the close button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloseBehaviour {
    /// Keep running in the tray.
    Tray,
    /// Quit Puddle.
    Quit,
}

impl From<settings::CloseBehaviour> for CloseBehaviour {
    fn from(c: settings::CloseBehaviour) -> Self {
        match c {
            settings::CloseBehaviour::Tray => Self::Tray,
            settings::CloseBehaviour::Quit => Self::Quit,
        }
    }
}

impl From<CloseBehaviour> for settings::CloseBehaviour {
    fn from(c: CloseBehaviour) -> Self {
        match c {
            CloseBehaviour::Tray => Self::Tray,
            CloseBehaviour::Quit => Self::Quit,
        }
    }
}

/// Preferences for Puddle's own window. `null` means Puddle's default.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UiPrefs {
    /// Light, dark or follow the system (default `system`).
    #[serde(default)]
    #[schema(required = true)]
    pub theme: Option<ThemeChoice>,
    /// Comfortable or compact spacing for the whole app (default `comfortable`). A request may
    /// leave it out, like every field here: out or `null` means the default.
    #[serde(default)]
    #[schema(required = true)]
    pub density: Option<DensityChoice>,
    /// The colour palette over the light or dark base (default `pond`).
    #[serde(default)]
    #[schema(required = true)]
    pub palette: Option<PaletteChoice>,
    /// The `custom` palette's accent colour, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_accent: Option<String>,
    /// The `custom` palette's page background, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_background: Option<String>,
    /// The `custom` palette's cards and panels, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_surface: Option<String>,
    /// The `custom` palette's text, `#rrggbb`; `null` keeps Pond's.
    #[serde(default)]
    #[schema(required = true, pattern = "^#[0-9a-fA-F]{6}$")]
    pub custom_text: Option<String>,
    /// A system notification for a new request (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notifications: Option<bool>,
    /// The system sound with a notification (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub sound: Option<bool>,
    /// What the close button does (default `tray`).
    #[serde(default)]
    #[schema(required = true)]
    pub close_behaviour: Option<CloseBehaviour>,
    /// Minimizing hides the window to the tray instead of the taskbar (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub minimize_to_tray: Option<bool>,
    /// The icon of the window, the tray and the taskbar (default `duck`). `custom` needs an
    /// uploaded image.
    #[serde(default)]
    #[schema(required = true)]
    pub app_icon: Option<AppIconChoice>,
    /// Notify only while Puddle's window is not focused (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_only_unfocused: Option<bool>,
    /// The waiting count on the tray, taskbar and window title (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub badge: Option<bool>,
    /// Notify when a workspace is killed for running out of memory (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_oom: Option<bool>,
    /// Notify when a workspace's root disk or volume reaches `disk_warning_percent` (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_disk_full: Option<bool>,
    /// How full a workspace's root disk or volume gets, in percent, before Puddle warns on the
    /// workspace and, with `notify_disk_full`, notifies; 50 to 100 (default 95).
    #[serde(default)]
    #[schema(required = true, minimum = 50, maximum = 100)]
    pub disk_warning_percent: Option<u8>,
    /// Notify when a workspace stops or crashes without being asked to (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_unexpected_stop: Option<bool>,
    /// Ask before a decision creates a rule for every workspace (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub confirm_global_rules: Option<bool>,
    /// The inbox's single-key shortcuts A, D, J and K (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub inbox_shortcuts: Option<bool>,
    /// Start Puddle when the user signs in to the computer (default off).
    #[serde(default)]
    #[schema(required = true)]
    pub start_at_login: Option<bool>,
    /// Notify when the free space on the drive that holds Puddle's data falls under the warning or
    /// the failure line (default on).
    #[serde(default)]
    #[schema(required = true)]
    pub notify_low_disk_space: Option<bool>,
    /// The free space, in GB (1024 x 1024 x 1024 bytes), under which `puddle doctor` warns and the
    /// app says so; 1 to 10000, default 20.
    #[serde(default)]
    #[schema(required = true, minimum = 1, maximum = 10000)]
    pub disk_warn_free_gb: Option<u32>,
    /// The free space, in GB, under which `puddle doctor` fails; 1 to 10000, default 5, never above
    /// the warning line.
    #[serde(default)]
    #[schema(required = true, minimum = 1, maximum = 10000)]
    pub disk_fail_free_gb: Option<u32>,
}

impl From<&settings::UiPrefs> for UiPrefs {
    fn from(u: &settings::UiPrefs) -> Self {
        Self {
            theme: u.theme.map(Into::into),
            density: u.density.map(Into::into),
            palette: u.palette.map(Into::into),
            custom_accent: colour_text(u.custom_accent.as_ref()),
            custom_background: colour_text(u.custom_background.as_ref()),
            custom_surface: colour_text(u.custom_surface.as_ref()),
            custom_text: colour_text(u.custom_text.as_ref()),
            notifications: u.notifications,
            sound: u.sound,
            close_behaviour: u.close_behaviour.map(Into::into),
            minimize_to_tray: u.minimize_to_tray,
            app_icon: u.app_icon.map(Into::into),
            notify_only_unfocused: u.notify_only_unfocused,
            badge: u.badge,
            notify_oom: u.notify_oom,
            notify_disk_full: u.notify_disk_full,
            disk_warning_percent: u
                .disk_warning_percent
                .map(settings::DiskWarningPercent::get),
            notify_unexpected_stop: u.notify_unexpected_stop,
            confirm_global_rules: u.confirm_global_rules,
            inbox_shortcuts: u.inbox_shortcuts,
            start_at_login: u.start_at_login,
            notify_low_disk_space: u.notify_low_disk_space,
            disk_warn_free_gb: u.disk_warn_free_gb.map(Into::into),
            disk_fail_free_gb: u.disk_fail_free_gb.map(Into::into),
        }
    }
}

fn colour_text(colour: Option<&settings::HexColour>) -> Option<String> {
    colour.map(|c| c.as_str().to_owned())
}

fn free_gb(gb: Option<u32>) -> Result<Option<settings::FreeGb>, ApiError> {
    gb.map(settings::FreeGb::new)
        .transpose()
        .map_err(ApiError::invalid)
}

fn parse_colour(text: Option<String>) -> Result<Option<settings::HexColour>, ApiError> {
    text.map(|t| settings::HexColour::new(&t))
        .transpose()
        .map_err(ApiError::invalid)
}

impl UiPrefs {
    fn apply_to(self, prefs: &mut settings::UiPrefs) -> Result<(), ApiError> {
        let Self {
            theme,
            density,
            palette,
            custom_accent,
            custom_background,
            custom_surface,
            custom_text,
            notifications,
            sound,
            close_behaviour,
            minimize_to_tray,
            app_icon,
            notify_only_unfocused,
            badge,
            notify_oom,
            notify_disk_full,
            disk_warning_percent,
            notify_unexpected_stop,
            confirm_global_rules,
            inbox_shortcuts,
            start_at_login,
            notify_low_disk_space,
            disk_warn_free_gb,
            disk_fail_free_gb,
        } = self;
        prefs.theme = theme.map(Into::into);
        prefs.density = density.map(Into::into);
        prefs.palette = palette.map(Into::into);
        prefs.custom_accent = parse_colour(custom_accent)?;
        prefs.custom_background = parse_colour(custom_background)?;
        prefs.custom_surface = parse_colour(custom_surface)?;
        prefs.custom_text = parse_colour(custom_text)?;
        prefs.notifications = notifications;
        prefs.sound = sound;
        prefs.close_behaviour = close_behaviour.map(Into::into);
        prefs.minimize_to_tray = minimize_to_tray;
        prefs.app_icon = app_icon.map(Into::into);
        prefs.notify_only_unfocused = notify_only_unfocused;
        prefs.badge = badge;
        prefs.notify_oom = notify_oom;
        prefs.notify_disk_full = notify_disk_full;
        prefs.disk_warning_percent = disk_warning_percent
            .map(settings::DiskWarningPercent::new)
            .transpose()
            .map_err(ApiError::invalid)?;
        prefs.notify_unexpected_stop = notify_unexpected_stop;
        prefs.confirm_global_rules = confirm_global_rules;
        prefs.inbox_shortcuts = inbox_shortcuts;
        prefs.start_at_login = start_at_login;
        prefs.notify_low_disk_space = notify_low_disk_space;
        prefs.disk_warn_free_gb = free_gb(disk_warn_free_gb)?;
        prefs.disk_fail_free_gb = free_gb(disk_fail_free_gb)?;
        // The failure line above the warning line would make the warning unreachable.
        settings::DiskSpaceLimits::new(
            prefs
                .disk_warn_free_gb
                .unwrap_or(settings::FreeGb::DEFAULT_WARN),
            prefs
                .disk_fail_free_gb
                .unwrap_or(settings::FreeGb::DEFAULT_FAIL),
        )
        .map_err(ApiError::invalid)?;
        Ok(())
    }
}

/// Which level an effective value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingSource {
    /// The workspace's override.
    Workspace,
    /// The global setting.
    Global,
    /// Puddle's built-in default.
    Default,
}

impl From<settings::Source> for SettingSource {
    fn from(source: settings::Source) -> Self {
        match source {
            settings::Source::Workspace => Self::Workspace,
            settings::Source::Global => Self::Global,
            settings::Source::Default => Self::Default,
        }
    }
}

/// An effective on/off value and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResolvedBool {
    /// The value.
    pub value: bool,
    /// Where it came from.
    pub source: SettingSource,
}

/// An effective number (MiB or seconds, see the field) and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResolvedNumber {
    /// The value.
    pub value: u32,
    /// Where it came from.
    pub source: SettingSource,
}

/// The effective clipboard-read setting and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ResolvedClipboardRead {
    /// The value.
    pub value: ClipboardRead,
    /// Where it came from.
    pub source: SettingSource,
}

fn rb(r: settings::Resolved<bool>) -> ResolvedBool {
    ResolvedBool {
        value: r.value,
        source: r.source.into(),
    }
}

/// The effective local toggles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EffectiveToggles {
    /// Loopback.
    pub loopback: ResolvedBool,
    /// Private networks.
    pub private: ResolvedBool,
    /// Link-local.
    pub link_local: ResolvedBool,
    /// Cloud metadata.
    pub metadata: ResolvedBool,
    /// Other special-purpose ranges.
    pub special: ResolvedBool,
    /// The company network.
    pub company_network: ResolvedBool,
}

/// The values a workspace gets: its override, else the global value, else Puddle's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EffectiveSettings {
    /// Guest memory in MiB.
    pub memory: ResolvedNumber,
    /// Guest vCPUs.
    pub cpus: ResolvedNumber,
    /// Local destination toggles.
    pub local_toggles: EffectiveToggles,
    /// Whether suffix rules reach local addresses.
    pub wildcards_reach_local: ResolvedBool,
    /// Reconnection grace in seconds.
    pub reconnection_grace: ResolvedNumber,
    /// Zoom hotkeys.
    pub zoom_hotkeys: ResolvedBool,
    /// Clipboard reads.
    pub clipboard_read: ResolvedClipboardRead,
    /// Whether direct SSH is on.
    pub direct_ssh: ResolvedBool,
    /// Whether logins made inside the workspace are captured.
    pub capture_logins: ResolvedBool,
    /// Whether a credential Puddle did not add raises a warning.
    pub warn_foreign_credentials: ResolvedBool,
}

impl From<settings::Effective> for EffectiveSettings {
    fn from(e: settings::Effective) -> Self {
        let settings::Effective {
            memory,
            cpus,
            local_toggles,
            wildcards_reach_local,
            reconnection_grace,
            zoom_hotkeys,
            clipboard_read,
            direct_ssh,
            capture_logins,
            warn_foreign_credentials,
        } = e;
        let settings::EffectiveToggles {
            loopback,
            private,
            link_local,
            metadata,
            special,
            company_network,
        } = local_toggles;
        Self {
            memory: ResolvedNumber {
                value: memory.value.get(),
                source: memory.source.into(),
            },
            cpus: ResolvedNumber {
                value: u32::from(cpus.value.get()),
                source: cpus.source.into(),
            },
            local_toggles: EffectiveToggles {
                loopback: rb(loopback),
                private: rb(private),
                link_local: rb(link_local),
                metadata: rb(metadata),
                special: rb(special),
                company_network: rb(company_network),
            },
            wildcards_reach_local: rb(wildcards_reach_local),
            reconnection_grace: ResolvedNumber {
                value: reconnection_grace.value.secs(),
                source: reconnection_grace.source.into(),
            },
            zoom_hotkeys: rb(zoom_hotkeys),
            clipboard_read: ResolvedClipboardRead {
                value: clipboard_read.value.into(),
                source: clipboard_read.source.into(),
            },
            direct_ssh: rb(direct_ssh),
            capture_logins: rb(capture_logins),
            warn_foreign_credentials: rb(warn_foreign_credentials),
        }
    }
}

/// The global settings, and what a workspace without overrides gets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GlobalSettingsView {
    /// Defaults for every workspace.
    pub workspace_defaults: SettingsLayer,
    /// VS Code server options.
    pub vscode_server: VsCodeServer,
    /// Preferences for Puddle's window.
    pub ui: UiPrefs,
    /// The tool switches (global only).
    pub tools: GlobalTools,
    /// The effective values for a workspace with no overrides.
    pub effective: EffectiveSettings,
    /// Fields in the stored document this Puddle doesn't know (written by a newer one); they
    /// are kept.
    pub unknown_fields: Vec<String>,
}

/// New global settings. Replaces every value listed here; unknown stored fields are kept.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct GlobalSettingsRequest {
    /// Defaults for every workspace.
    #[serde(default)]
    pub workspace_defaults: SettingsLayer,
    /// VS Code server options.
    #[serde(default)]
    pub vscode_server: VsCodeServer,
    /// Preferences for Puddle's window.
    #[serde(default)]
    pub ui: UiPrefs,
    /// The tool switches.
    #[serde(default)]
    pub tools: GlobalTools,
}

impl GlobalSettingsRequest {
    pub(crate) fn apply_to(self, global: &mut settings::GlobalSettings) -> Result<(), ApiError> {
        let Self {
            workspace_defaults,
            vscode_server:
                VsCodeServer {
                    server,
                    telemetry,
                    auto_update,
                },
            ui,
            tools: GlobalTools { mise },
        } = self;
        global.tools.mise = mise;
        workspace_defaults.apply_to(&mut global.workspace_defaults)?;
        global.vscode_server.server = server.map(Into::into);
        global.vscode_server.telemetry = telemetry;
        global.vscode_server.auto_update = auto_update;
        ui.apply_to(&mut global.ui)
    }
}

impl GlobalSettingsView {
    pub(crate) fn new(loaded: &settings::Loaded<settings::GlobalSettings>) -> Self {
        let global = &loaded.settings;
        Self {
            workspace_defaults: (&global.workspace_defaults).into(),
            vscode_server: VsCodeServer {
                server: global.vscode_server.server.map(Into::into),
                telemetry: global.vscode_server.telemetry,
                auto_update: global.vscode_server.auto_update,
            },
            ui: (&global.ui).into(),
            tools: GlobalTools {
                mise: global.tools.mise,
            },
            effective: settings::resolve(global, None).into(),
            unknown_fields: loaded.unknown_fields.clone(),
        }
    }
}

/// One workspace's overrides and effective values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WorkspaceSettingsView {
    /// The workspace.
    pub workspace: WorkspaceId,
    /// Its overrides; `null` inherits.
    pub overrides: SettingsLayer,
    /// Its own tool switches; `null` is the default.
    pub tools: WorkspaceTools,
    /// What it gets.
    pub effective: EffectiveSettings,
    /// Unknown fields in its stored document (kept).
    pub unknown_fields: Vec<String>,
}

/// New overrides for one workspace. Replaces every value listed; `null` inherits.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSettingsRequest {
    /// The overrides.
    #[serde(default)]
    pub overrides: SettingsLayer,
    /// Its tool switches.
    #[serde(default)]
    pub tools: WorkspaceTools,
}

// ---------------------------------------------------------------------------------------------
// Consents

/// What the user agreed to or declined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Consent {
    /// Never asked.
    NotAsked,
    /// Agreed.
    Granted {
        /// Epoch ms.
        at: u64,
        /// The terms version shown.
        terms_version: String,
    },
    /// Declined.
    Declined {
        /// Epoch ms.
        at: u64,
        /// The terms version shown.
        terms_version: String,
    },
}

impl From<&settings::Consent> for Consent {
    fn from(c: &settings::Consent) -> Self {
        match c {
            settings::Consent::NotAsked => Self::NotAsked,
            settings::Consent::Granted { at, terms_version } => Self::Granted {
                at: at.0,
                terms_version: terms_version.to_string(),
            },
            settings::Consent::Declined { at, terms_version } => Self::Declined {
                at: at.0,
                terms_version: terms_version.to_string(),
            },
        }
    }
}

/// Every consent Puddle asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Consents {
    /// Usage telemetry.
    pub telemetry: Consent,
    /// Crash reports.
    pub crash_reports: Consent,
    /// Microsoft's VS Code server and its licence terms.
    pub vscode_server: Consent,
}

impl From<&settings::Consents> for Consents {
    fn from(c: &settings::Consents) -> Self {
        Self {
            telemetry: c.get(settings::ConsentKind::Telemetry).into(),
            crash_reports: c.get(settings::ConsentKind::CrashReports).into(),
            vscode_server: c.get(settings::ConsentKind::VsCodeServer).into(),
        }
    }
}

/// Which consent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConsentKind {
    /// Usage telemetry.
    Telemetry,
    /// Crash reports.
    CrashReports,
    /// Microsoft's VS Code server.
    VscodeServer,
}

impl From<ConsentKind> for settings::ConsentKind {
    fn from(kind: ConsentKind) -> Self {
        match kind {
            ConsentKind::Telemetry => Self::Telemetry,
            ConsentKind::CrashReports => Self::CrashReports,
            ConsentKind::VscodeServer => Self::VsCodeServer,
        }
    }
}

/// The user's answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConsentDecision {
    /// Agreed.
    Granted,
    /// Declined.
    Declined,
}

/// Records the user's answer to a consent prompt. Puddle stamps the time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConsentRequest {
    /// Agreed or declined.
    pub decision: ConsentDecision,
    /// The version of the terms shown (a date or a licence URL; 1 to 256 visible ASCII
    /// characters).
    #[schema(min_length = 1, max_length = 256)]
    pub terms_version: String,
}

impl ConsentRequest {
    pub(crate) fn into_consent(self, now_ms: u64) -> Result<settings::Consent, ApiError> {
        let terms_version =
            settings::TermsVersion::new(&self.terms_version).map_err(ApiError::invalid)?;
        let at = settings::UnixMillis(now_ms);
        Ok(match self.decision {
            ConsentDecision::Granted => settings::Consent::Granted { at, terms_version },
            ConsentDecision::Declined => settings::Consent::Declined { at, terms_version },
        })
    }
}

#[cfg(test)]
mod tests;
