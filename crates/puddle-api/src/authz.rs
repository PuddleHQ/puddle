// SPDX-License-Identifier: GPL-3.0-or-later
//! Who may do what: the principal behind a request, the action and resource every route
//! declares, and the one check between the guard and the handler.
//!
//! The guard ([`crate::auth`]) says who is asking and puts a [`Principal`] on the request. Every
//! route is listed in [`declaration`] with its [`Action`] and the [`ResourceRef`] its path names;
//! [`authorize`] looks the route up and asks the [`Authorizer`]. A route that is missing from the
//! list is refused, so a new route cannot go out unchecked (a test also walks every route).
//!
//! The rule ([`Isolation`]): a user reaches only their own workspaces and what hangs off them
//! (pending requests, rules, audit records, events) and their own collections (identities, Git
//! defaults, global environment, rule sets and global rules, pasted tokens). The owner is a user
//! like any other here, with no way into another user's; the owner alone has the install's own
//! settings and the accounts signed in on the computer itself. A resource that is someone else's
//! answers exactly as one that does not exist: by-id routes of a collection ask the store for the
//! principal's own, so another user's id is unknown there.

use std::sync::Arc;

use axum::extract::{FromRequestParts, MatchedPath, RawPathParams, Request, State};
use axum::http::request::Parts;
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use puddle_store::{DeviceId, Role, Store, StoreError, UserId, WorkspaceOwners};
use puddle_types::{PendingId, RuleId, WorkspaceId};

use crate::error::{ApiError, ErrorCode, blocking};
use crate::routes::AppState;
use crate::workspaces::WorkspaceError;

/// How a principal reached the API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Via {
    /// The install token: the desktop window or the CLI on the server itself.
    Install,
    /// A signed-in browser.
    Device(DeviceId),
}

/// Who is asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Principal {
    pub(crate) user: UserId,
    pub(crate) role: Role,
    pub(crate) via: Via,
}

impl Principal {
    /// The holder of the install token: the owner.
    pub(crate) const INSTALL: Self = Self {
        user: UserId::OWNER,
        role: Role::Owner,
        via: Via::Install,
    };
}

/// The principal the guard put on the request. A route that runs without one is a wiring
/// mistake, and is refused like a missing token.
impl<S: Send + Sync> FromRequestParts<S> for Principal {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<Self>()
                .copied()
                .ok_or_else(unauthorized),
        )
    }
}

/// What a route does. Grouped so a later permission system can grant them one by one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Action {
    /// Who is answering, the contract, the event stream and the problems (both kept to the
    /// principal).
    ReadService,
    /// The install's own settings and state: global settings, consents, the first-run flow, the
    /// system check, the network, app icons, decrypting settings, orphan volumes.
    ReadInstall,
    ChangeInstall,
    ListWorkspaces,
    /// Making a workspace, and checking the hosts of its repositories first.
    CreateWorkspace,
    ReadWorkspace,
    ChangeWorkspace,
    DeleteWorkspace,
    /// Starting, stopping, attaching, terminals, ports: using a workspace without changing what
    /// it is.
    UseWorkspace,
    /// A workspace's identities, repository table and push and pull switches.
    ReadWorkspaceGit,
    ChangeWorkspaceGit,
    ReadRequests,
    DecideRequests,
    ReadRules,
    /// Global rules and rule sets.
    ReadGlobalRules,
    /// Rules of one workspace (allow and deny, method and path).
    ChangeRules,
    /// Global rules, rule sets and their switches.
    ChangeGlobalRules,
    ReadAudit,
    ReadIdentities,
    ChangeIdentities,
    ReadGlobalEnvironment,
    ChangeGlobalEnvironment,
    /// The principal's pasted tokens: keeping one, removing one, checking a credential or reading
    /// its account's profile, the repositories their identities reach. A source other than the
    /// principal's own pasted token is the owner's alone (see [`crate::stored_ids`]).
    UseCredentials,
    /// Signing in before there is a token (the page's first visit, a password, an invite, the
    /// session cookie): no principal yet, and the routes answer only a public address.
    SignIn,
    /// The signed-in user's own sessions: their devices and their password.
    ManageSessions,
    /// The accounts signed in on this computer (the GitHub CLI's, Git's credential helper's):
    /// listing them and signing in. They are the operator's.
    UseComputerAccounts,
    /// The user list and its changes (the admin screen).
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the admin screen's routes use it")
    )]
    ManageUsers,
    /// Serving the API beyond this computer.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the remote serving setting uses it")
    )]
    SetRemoteServing,
}

impl Action {
    /// Only the owner may do it: the install's own things.
    pub(crate) fn owner_only(self) -> bool {
        match self {
            Self::ReadService
            | Self::ListWorkspaces
            | Self::ReadWorkspace
            | Self::ChangeWorkspace
            | Self::DeleteWorkspace
            | Self::UseWorkspace
            | Self::ReadRequests
            | Self::DecideRequests
            | Self::ReadRules
            | Self::ChangeRules
            | Self::ReadAudit
            | Self::SignIn
            | Self::ManageSessions
            | Self::CreateWorkspace
            | Self::ReadWorkspaceGit
            | Self::ChangeWorkspaceGit
            | Self::ReadGlobalRules
            | Self::ChangeGlobalRules
            | Self::ReadIdentities
            | Self::ChangeIdentities
            | Self::ReadGlobalEnvironment
            | Self::ChangeGlobalEnvironment
            | Self::UseCredentials => false,
            Self::ReadInstall
            | Self::ChangeInstall
            | Self::UseComputerAccounts
            | Self::ManageUsers
            | Self::SetRemoteServing => true,
        }
    }

    /// Only with the install token: a signed-in browser must never widen who can reach Puddle.
    pub(crate) fn needs_install_token(self) -> bool {
        matches!(self, Self::SetRemoteServing)
    }
}

/// What a request acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ResourceRef {
    /// Nobody's: who is answering, the contract.
    Service,
    /// The install's own (owner only).
    Install,
    /// The principal's own collection: a list the route filters to the principal, or a create.
    Own,
    Workspace(WorkspaceId),
    Pending(PendingId),
    Rule(RuleId),
    RequestRule(i64),
}

/// Why a check failed.
#[derive(Debug)]
pub(crate) enum Denial {
    /// Not the principal's: answered as the same resource not existing would be.
    NotFound(ApiError),
    /// Only the owner may do this.
    OwnerOnly,
    /// This needs the install token: the desktop window or the CLI on the server itself.
    InstallTokenOnly,
    /// The owner could not be looked up.
    Failed(StoreError),
}

impl From<Denial> for ApiError {
    fn from(denial: Denial) -> Self {
        match denial {
            Denial::NotFound(err) => err,
            Denial::OwnerOnly => Self::new(
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "only the owner of this Puddle can do this; ask them",
            ),
            Denial::InstallTokenOnly => Self::new(
                StatusCode::FORBIDDEN,
                ErrorCode::Forbidden,
                "this can only be done from the desktop window or the command line on the server",
            ),
            Denial::Failed(err) => Self::internal(&err),
        }
    }
}

/// Decides whether a principal may do an action on a resource.
pub(crate) trait Authorizer: Send + Sync {
    /// `Ok` when allowed. Blocking: it may read the store.
    ///
    /// # Errors
    /// The [`Denial`].
    fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<(), Denial>;
}

/// Every user isolated from every other one; see the module docs.
pub(crate) struct Isolation {
    store: Arc<Store>,
}

impl Isolation {
    pub(crate) fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// The user `resource` belongs to, and the answer for someone it does not belong to (the
    /// same as for an id that does not exist). `None` for a resource that is no user's.
    fn owner(&self, resource: &ResourceRef) -> Result<Option<(UserId, ApiError)>, StoreError> {
        Ok(Some(match resource {
            ResourceRef::Service | ResourceRef::Install | ResourceRef::Own => return Ok(None),
            ResourceRef::Workspace(id) => (
                self.store.workspace_owner(id)?,
                ApiError::from(WorkspaceError::NotFound(format!("no workspace {id}"))),
            ),
            ResourceRef::Pending(id) => {
                let user = match self.store.pending(*id) {
                    Ok(row) => self.store.workspace_owner(&row.workspace)?,
                    Err(StoreError::UnknownPending(_)) => UserId::OWNER,
                    Err(err) => return Err(err),
                };
                (user, ApiError::from(StoreError::UnknownPending(*id)))
            }
            // An unknown rule resolves to the owner, so for anyone else it answers as another
            // user's would.
            ResourceRef::Rule(id) => {
                let rule = self.store.rules().into_iter().find(|r| r.id == *id);
                (
                    rule.map_or(UserId::OWNER, |r| r.user),
                    ApiError::from(StoreError::UnknownRule(*id)),
                )
            }
            ResourceRef::RequestRule(id) => {
                let rule = self
                    .store
                    .request_rules(None)?
                    .into_iter()
                    .find(|r| r.id == *id);
                (
                    rule.map_or(UserId::OWNER, |r| r.user),
                    ApiError::from(StoreError::UnknownRequestRule(*id)),
                )
            }
        }))
    }
}

impl Authorizer for Isolation {
    fn check(
        &self,
        principal: &Principal,
        action: Action,
        resource: &ResourceRef,
    ) -> Result<(), Denial> {
        if action.needs_install_token() && principal.via != Via::Install {
            return Err(Denial::InstallTokenOnly);
        }
        let owner_only = action.owner_only() || *resource == ResourceRef::Install;
        if owner_only && principal.role != Role::Owner {
            return Err(Denial::OwnerOnly);
        }
        match self.owner(resource).map_err(Denial::Failed)? {
            Some((user, _)) if user == principal.user => Ok(()),
            Some((_, not_found)) => Err(Denial::NotFound(not_found)),
            None => Ok(()),
        }
    }
}

/// [`Authorizer::check`] from the route layer or a handler (for a resource the body names: a
/// rule's workspace, a global scope).
///
/// # Errors
/// The denial as the response it becomes.
pub(crate) async fn check(
    state: &AppState,
    principal: Principal,
    action: Action,
    resource: ResourceRef,
) -> Result<(), ApiError> {
    let authorizer = state.authorizer.clone();
    // A resource that is nobody's needs no lookup, so it is checked at once: an event stream
    // subscribes in the same turn as its request, before anything emitted meanwhile is missed.
    if matches!(
        resource,
        ResourceRef::Service | ResourceRef::Install | ResourceRef::Own
    ) {
        return Ok(authorizer.check(&principal, action, &resource)?);
    }
    blocking(move || Ok(authorizer.check(&principal, action, &resource)?)).await
}

/// Who owns which workspace, for keeping a list to one principal's.
///
/// # Errors
/// A database error, as an internal error.
pub(crate) async fn owners(state: &AppState) -> Result<WorkspaceOwners, ApiError> {
    let store = state.store.clone();
    blocking(move || Ok(store.workspace_owners()?)).await
}

/// What making or changing a rule scoped to `workspace` (or to every workspace of the principal
/// when `None`) needs.
pub(crate) fn rule_scope(workspace: Option<&WorkspaceId>) -> (Action, ResourceRef) {
    match workspace {
        Some(id) => (Action::ChangeRules, ResourceRef::Workspace(id.clone())),
        None => (Action::ChangeGlobalRules, ResourceRef::Own),
    }
}

/// Which path parameter names the resource, and what kind it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Service,
    Install,
    Own,
    /// A workspace, by the named path parameter.
    Workspace(&'static str),
    Pending,
    Rule,
    RequestRule,
    /// The terminal's WebSocket: it carries a ticket, not the token. The ticket is made by
    /// `POST /api/workspaces/{id}/terminals` (checked as [`Action::UseWorkspace`] on that
    /// workspace) and names its workspace and terminal, so the socket needs no principal.
    Ticket,
}

/// What a route declares: the action and where its resource comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Declared {
    pub(crate) action: Action,
    pub(crate) target: Target,
}

/// The declaration of the route `method` `path` (axum's matched path, `{name}` for a parameter),
/// or `None` for one that has none and is therefore refused. One arm per action and resource.
#[expect(
    clippy::too_many_lines,
    reason = "every route in one place, one arm per action and resource"
)]
pub(crate) fn declaration(method: &Method, path: &str) -> Option<Declared> {
    use Action as A;
    use Target as T;
    const WS: Target = T::Workspace("id");
    let (action, target) = match (method.as_str(), path) {
        ("GET", "/api/health" | "/api/openapi.json") => (A::ReadService, T::Service),
        ("GET", "/api/events" | "/api/problems") => (A::ReadService, T::Own),
        ("GET", "/api/auth/status")
        | (
            "POST",
            "/api/auth/setup" | "/api/auth/sign-in" | "/api/auth/invite" | "/api/auth/refresh"
            | "/api/auth/sign-out",
        ) => (A::SignIn, T::Service),
        ("PUT", "/api/auth/password")
        | ("GET", "/api/devices")
        | ("DELETE", "/api/devices/{id}") => (A::ManageSessions, T::Own),
        (
            "GET",
            "/api/network-health"
            | "/api/doctor"
            | "/api/app-icons"
            | "/api/settings"
            | "/api/consents"
            | "/api/first-run"
            | "/api/proxy-sign-ins"
            | "/api/interception"
            | "/api/interception/learned-limits"
            | "/api/interception/never-decrypt"
            | "/api/orphan-volumes"
            | "/api/auth/methods",
        ) => (A::ReadInstall, T::Install),
        (
            "PUT",
            "/api/settings"
            | "/api/consents/{kind}"
            | "/api/first-run"
            | "/api/proxy-sign-ins"
            | "/api/app-icons/custom"
            | "/api/interception"
            | "/api/interception/learned-limits"
            | "/api/interception/never-decrypt"
            | "/api/auth/methods",
        )
        | ("DELETE", "/api/app-icons/custom" | "/api/proxy-sign-ins/{proxy}")
        | ("POST", "/api/dev-certificate/trust" | "/api/orphan-volumes/{id}/restore") => {
            (A::ChangeInstall, T::Install)
        }
        ("GET", "/api/workspaces" | "/api/workspaces/lookup") => (A::ListWorkspaces, T::Own),
        ("POST", "/api/workspaces" | "/api/repository-hosts") => (A::CreateWorkspace, T::Own),
        (
            "GET",
            "/api/workspaces/{id}"
            | "/api/workspaces/{id}/checkouts"
            | "/api/workspaces/{id}/delete-check"
            | "/api/workspaces/{id}/env"
            | "/api/workspaces/{id}/interception"
            | "/api/workspaces/{id}/interception/learned"
            | "/api/workspaces/{id}/logins"
            | "/api/workspaces/{id}/shell-init"
            | "/api/workspaces/{id}/ssh-agent",
        ) => (A::ReadWorkspace, WS),
        ("GET", "/api/settings/workspaces/{workspace}") => {
            (A::ReadWorkspace, T::Workspace("workspace"))
        }
        ("PUT", "/api/settings/workspaces/{workspace}") => {
            (A::ChangeWorkspace, T::Workspace("workspace"))
        }
        ("PATCH", "/api/workspaces/{id}")
        | (
            "PUT",
            "/api/workspaces/{id}/env/{name}"
            | "/api/workspaces/{id}/interception"
            | "/api/workspaces/{id}/session-user"
            | "/api/workspaces/{id}/shell-init"
            | "/api/workspaces/{id}/ssh-agent/{host}"
            | "/api/workspaces/{id}/start-folder",
        )
        | (
            "DELETE",
            "/api/workspaces/{id}/env/{name}"
            | "/api/workspaces/{id}/shell-init"
            | "/api/workspaces/{id}/interception/learned"
            | "/api/workspaces/{id}/interception/learned/{host}"
            | "/api/workspaces/{id}/logins/{service}"
            | "/api/workspaces/{id}/repositories/{entry}"
            | "/api/workspaces/{id}/ssh-agent/{host}",
        )
        | (
            "POST",
            "/api/workspaces/{id}/interception/learned/{host}/permanent"
            | "/api/workspaces/{id}/repositories"
            | "/api/workspaces/{id}/repositories/{entry}/clone"
            | "/api/workspaces/{id}/reset-root-disk"
            | "/api/workspaces/{id}/grow-root-disk",
        ) => (A::ChangeWorkspace, WS),
        ("DELETE", "/api/workspaces/{id}") => (A::DeleteWorkspace, WS),
        (
            "POST",
            "/api/workspaces/{id}/start"
            | "/api/workspaces/{id}/stop"
            | "/api/workspaces/{id}/reclaim"
            | "/api/workspaces/{id}/attach"
            | "/api/workspaces/{id}/terminals",
        )
        | ("GET", "/api/workspaces/{id}/ports")
        | ("PUT" | "DELETE", "/api/workspaces/{id}/ports/{port}") => (A::UseWorkspace, WS),
        ("GET", "/api/workspaces/{id}/terminals/{terminal}") => (A::UseWorkspace, T::Ticket),
        ("GET", "/api/workspaces/{id}/git") => (A::ReadWorkspaceGit, WS),
        (
            "PUT" | "POST",
            "/api/workspaces/{id}/identities"
            | "/api/workspaces/{id}/git/switches"
            | "/api/workspaces/{id}/git/repos",
        )
        | ("PUT" | "DELETE", "/api/workspaces/{id}/git/repos/{repo}")
        | ("DELETE", "/api/workspaces/{id}/identities/{identity}") => (A::ChangeWorkspaceGit, WS),
        ("GET", "/api/pending" | "/api/inbox") => (A::ReadRequests, T::Own),
        ("GET", "/api/pending/{id}") => (A::ReadRequests, T::Pending),
        ("POST", "/api/pending/{id}/approve" | "/api/pending/{id}/deny") => {
            (A::DecideRequests, T::Pending)
        }
        ("GET", "/api/rules" | "/api/request-rules") => (A::ReadRules, T::Own),
        ("POST", "/api/rules" | "/api/request-rules") => (A::ChangeRules, T::Own),
        ("DELETE", "/api/rules/{id}") | ("PUT", "/api/rules/{id}/expiry") => {
            (A::ChangeRules, T::Rule)
        }
        ("DELETE", "/api/request-rules/{id}") => (A::ChangeRules, T::RequestRule),
        ("GET", "/api/rule-sets") => (A::ReadGlobalRules, T::Own),
        ("POST", "/api/rule-sets")
        | ("PUT" | "DELETE", "/api/rule-sets/{id}")
        | ("PUT", "/api/rule-sets/{id}/switch") => (A::ChangeGlobalRules, T::Own),
        ("GET", "/api/audit") => (A::ReadAudit, T::Own),
        ("GET", "/api/identities" | "/api/identities/git-defaults" | "/api/identities/{id}") => {
            (A::ReadIdentities, T::Own)
        }
        ("POST", "/api/identities")
        | ("DELETE", "/api/identities/{id}")
        | (
            "PUT",
            "/api/identities/git-defaults"
            | "/api/identities/order"
            | "/api/identities/{id}"
            | "/api/identities/{id}/default",
        ) => (A::ChangeIdentities, T::Own),
        ("GET", "/api/env") => (A::ReadGlobalEnvironment, T::Own),
        ("PUT" | "DELETE", "/api/env/{name}") => (A::ChangeGlobalEnvironment, T::Own),
        ("GET", "/api/repos")
        | (
            "POST",
            "/api/credentials/check"
            | "/api/credentials/profile"
            | "/api/credentials/stored"
            | "/api/repos/refresh",
        )
        | ("DELETE", "/api/credentials/stored/{id}") => (A::UseCredentials, T::Own),
        ("GET", "/api/credentials/found") | ("POST", "/api/credentials/sign-in") => {
            (A::UseComputerAccounts, T::Install)
        }
        _ => return None,
    };
    Some(Declared { action, target })
}

/// The resource a declared route names, from its path parameters. `None` when the parameter does
/// not parse.
fn resource(target: Target, params: &RawPathParams) -> Option<ResourceRef> {
    let param = |name: &str| params.iter().find(|(k, _)| *k == name).map(|(_, v)| v);
    let id = || param("id")?.parse::<i64>().ok();
    Some(match target {
        Target::Service | Target::Ticket => ResourceRef::Service,
        Target::Install => ResourceRef::Install,
        Target::Own => ResourceRef::Own,
        Target::Workspace(name) => ResourceRef::Workspace(WorkspaceId::new(param(name)?).ok()?),
        Target::Pending => ResourceRef::Pending(PendingId(id()?)),
        Target::Rule => ResourceRef::Rule(RuleId(id()?)),
        Target::RequestRule => ResourceRef::RequestRule(id()?),
    })
}

fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        ErrorCode::Unauthorized,
        "missing or wrong bearer token",
    )
}

/// axum route middleware: the route's declaration, checked for the request's principal.
pub(crate) async fn authorize(
    State(state): State<AppState>,
    matched: Option<MatchedPath>,
    params: RawPathParams,
    request: Request,
    next: Next,
) -> Response {
    let path = matched.as_ref().map_or("", MatchedPath::as_str);
    // axum answers `HEAD` from the `GET` route; it is the same read.
    let method = if request.method() == Method::HEAD {
        &Method::GET
    } else {
        request.method()
    };
    let Some(declared) = declaration(method, path) else {
        tracing::error!(method = %request.method(), path, "route has no permission declaration");
        return ApiError::internal(&"route has no permission declaration").into_response();
    };
    if declared.target == Target::Ticket || declared.action == Action::SignIn {
        return next.run(request).await;
    }
    let Some(principal) = request.extensions().get::<Principal>().copied() else {
        return unauthorized().into_response();
    };
    // An id that does not parse names nothing, and the handler refuses it; the role and the way
    // in are still checked.
    let resource = resource(declared.target, &params).unwrap_or(ResourceRef::Own);
    match check(&state, principal, declared.action, resource).await {
        Ok(()) => next.run(request).await,
        Err(err) => err.into_response(),
    }
}

#[cfg(test)]
mod tests;
