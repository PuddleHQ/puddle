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

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

mod app_icon;
mod audit;
mod audit_record;
mod auth;
mod consent;
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
mod rules;
mod settings_view;
mod shell_init;
mod start_folder;
mod workspaces;

pub use app_icon::{AppIconChoice, AppIcons, BuiltInIcon, CustomIcon};
pub use audit::{
    AuditEntry, AuditOutcome, AuditPage, AuditPending, AuditRequestRule, AuditRule, AuditRuleSet,
    AuditType, ConnectionDecision, ConnectionOrigin, ConnectionOutcome, PendingExpiryReason,
    RuleDeleteReason, WorkspaceOpKind, WorkspaceOpOutcome,
};
pub use audit_record::AuditRecord;
pub use auth::{
    AuthStatus, ChangePasswordRequest, DeviceList, DeviceView, InviteRequest,
    PasswordSignInRequest, SessionGrant, SetupRequest, SignInMethodKind, SignInMethodRequest,
    SignInMethodView, SignInMethods, UserView,
};
pub use consent::{Consent, ConsentDecision, ConsentKind, ConsentRequest, Consents};
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
pub use rules::{
    Actor, DecisionOutcome, DecisionRequest, Effect, Inbox, InboxGroup, NewRuleRequest,
    PatternKind, PendingList, PendingRequest, PendingState, Rule, RuleExpiryRequest, RuleList,
    RuleScope, ScopeChoice,
};
pub use settings_view::{
    ClipboardRead, CloseBehaviour, DensityChoice, EffectiveSettings, EffectiveToggles,
    GlobalSettingsRequest, GlobalSettingsView, LocalToggles, PaletteChoice, ResolvedBool,
    ResolvedClipboardRead, ResolvedNumber, ServerChoice, SettingSource, SettingsLayer, ThemeChoice,
    UiPrefs, VsCodeServer, WorkspaceSettingsRequest, WorkspaceSettingsView,
};
pub use shell_init::{ShellInitRequest, ShellInitSaved, ShellInitView};
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

#[cfg(test)]
mod tests;
