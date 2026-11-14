// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hosts of a create's repositories: what the create form shows before it allows them, and
//! the check that refuses a create a block rule covers (storage ST-44a).

use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_store::{RepoRef, Store, UserId};
use puddle_types::{Decision, Host, WorkspaceId};

use crate::ApiErrorBody;
use crate::authz::{self, Action, Principal, ResourceRef};
use crate::error::{ApiError, ErrorCode, blocking};
use crate::routes::AppState;
use crate::routes::workspaces::workspace_id;
use crate::wire::{RepositoryHost, RepositoryHostState, RepositoryHosts, RepositoryHostsRequest};

/// Stands for a workspace that does not exist yet: no workspace rule or switch can name it, so
/// the rules answer as they do for any new workspace (a 40-bit id; a clash is one in 10^12).
fn new_workspace() -> WorkspaceId {
    WorkspaceId::from_random([0xff; 5])
}

/// The hosts of the HTTPS repository addresses, each once, in first-seen order. An address that
/// is no repository (an SSH remote, a bad one) has none: the create refuses it by itself.
pub(crate) fn hosts_of<'a>(urls: impl IntoIterator<Item = &'a str>) -> Vec<Host> {
    let mut hosts: Vec<Host> = Vec::new();
    for url in urls {
        let host = RepoRef::from_https_url(url)
            .ok()
            .and_then(|repo| Host::parse_normalised(repo.host.as_str()).ok());
        if let Some(host) = host
            && !hosts.contains(&host)
        {
            hosts.push(host);
        }
    }
    hosts
}

/// Which rule or rule set of `user` blocks a host, in words, for a decision that is a deny.
fn blocked_by(store: &Store, user: UserId, host: &Host, decision: Decision) -> Option<String> {
    match decision {
        Decision::Deny { rule_id, .. } => Some(format!("your rule {rule_id} blocks {host}")),
        Decision::SetDeny { set, .. } => {
            let name = store
                .rule_set(user, set)
                .map_or_else(|_| set.to_string(), |info| info.name);
            Some(format!("the rule set {name} blocks {host}"))
        }
        _ => None,
    }
}

/// Where each host stands for `workspace` of `user` (a workspace of theirs that does not exist
/// yet when `None`).
pub(crate) fn stand(
    store: &Store,
    user: UserId,
    workspace: Option<&WorkspaceId>,
    hosts: &[Host],
) -> Vec<RepositoryHost> {
    let placeholder = new_workspace();
    hosts
        .iter()
        .map(|host| {
            let decision = store
                .as_user(user)
                .peek(workspace.unwrap_or(&placeholder), host);
            let (state, blocked_by) = match decision {
                None => (RepositoryHostState::New, None),
                Some(d) => match blocked_by(store, user, host, d) {
                    Some(text) => (RepositoryHostState::Blocked, Some(text)),
                    None => (RepositoryHostState::Allowed, None),
                },
            };
            RepositoryHost {
                host: host.to_string(),
                state,
                blocked_by,
            }
        })
        .collect()
}

/// Refuses (409) a create or an added repository whose host a block covers: the create never
/// overrides a block, so the list or the rule has to change first.
pub(crate) async fn refuse_blocked(
    store: &Arc<Store>,
    user: UserId,
    workspace: Option<WorkspaceId>,
    hosts: Vec<Host>,
) -> Result<(), ApiError> {
    let store = store.clone();
    let stands = blocking(move || Ok(stand(&store, user, workspace.as_ref(), &hosts))).await?;
    let blocked: Vec<String> = stands.into_iter().filter_map(|h| h.blocked_by).collect();
    if blocked.is_empty() {
        return Ok(());
    }
    Err(ApiError::new(
        StatusCode::CONFLICT,
        ErrorCode::Conflict,
        format!(
            "{}; change the repository list or lift the rule first, a create never overrides a block",
            blocked.join("; ")
        ),
    ))
}

/// Says which hosts the repositories of a create form are on and where each stands: `new` hosts
/// are the ones the create would allow for the workspace, `blocked` ones refuse the create.
#[utoipa::path(
    post,
    path = "/api/repository-hosts",
    tag = "workspaces",
    request_body = RepositoryHostsRequest,
    responses(
        (status = OK, description = "the hosts and where each stands", body = RepositoryHosts),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a bad workspace id or more than 100 addresses", body = ApiErrorBody)
    )
)]
pub(crate) async fn repository_hosts(
    State(state): State<AppState>,
    principal: Principal,
    crate::extract::Json(body): crate::extract::Json<RepositoryHostsRequest>,
) -> Result<Json<RepositoryHosts>, ApiError> {
    if body.urls.len() > crate::workspaces::MAX_REPOSITORIES {
        return Err(ApiError::invalid("too many repository addresses"));
    }
    let workspace = body.workspace.as_deref().map(workspace_id).transpose()?;
    if let Some(id) = &workspace {
        // The body names the workspace, so the route's own check could not see it.
        authz::check(
            &state,
            principal,
            Action::ChangeWorkspace,
            ResourceRef::Workspace(id.clone()),
        )
        .await?;
        state.workspaces.get(id).await?;
    }
    let hosts = hosts_of(body.urls.iter().map(String::as_str));
    let store = state.store.clone();
    let user = principal.user;
    let hosts = blocking(move || Ok(stand(&store, user, workspace.as_ref(), &hosts))).await?;
    Ok(Json(RepositoryHosts { hosts }))
}
