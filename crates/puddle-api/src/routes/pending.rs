// SPDX-License-Identifier: GPL-3.0-or-later
//! Pending requests: list, inbox, approve, deny (docs/spec/rules.md §3).

use axum::Json;
use axum::extract::State;
use std::collections::HashMap;

use puddle_netpolicy::{CompanySignals, Target, classify_with_signals};
use puddle_settings::resolve;
use puddle_store::{Actor, PendingRow, PendingState, ScopeChoice};
use puddle_types::{Host, LocalCategory, PendingId, WorkspaceId};
use serde::Deserialize;

use crate::ApiErrorBody;
use crate::authz::{self, Action, Principal, ResourceRef};
use crate::error::{ApiError, blocking};
use crate::extract::{Path, Query};
use crate::routes::AppState;
use crate::routes::settings::{load_global, load_workspace};
use crate::wire::{
    DecisionOutcome, DecisionRequest, Effect, Inbox, InboxGroup, PendingList, PendingRequest,
};

/// Which open requests to list.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingQuery {
    /// Only this workspace's requests. All workspaces if left out.
    workspace: Option<WorkspaceId>,
}

/// The local category a destination is in by itself: an IP literal's class, or a name such as
/// `localhost` or the metadata names. Other names are classified by what they resolve to, which
/// Puddle only knows when the connection is made.
fn local_category(host: &Host, signals: &dyn CompanySignals) -> Option<LocalCategory> {
    match host {
        Host::Ip(ip) => classify_with_signals(*ip, None, signals).category(),
        Host::Name(_) => Target::from_host(host.clone()).named_category(),
    }
}

/// The signal that put an IP literal on the company network, as its audit code.
fn company_signal(host: &Host, signals: &dyn CompanySignals) -> Option<&'static str> {
    match host {
        Host::Ip(ip) => match classify_with_signals(*ip, None, signals) {
            puddle_netpolicy::AddressClass::Company(signal) => Some(signal.code()),
            _ => None,
        },
        Host::Name(_) => None,
    }
}

/// Maps rows to wire requests, naming for each open one the local-destination toggle that blocks
/// its approval. Settings are read only if some open row is to a local destination; a workspace
/// whose settings can't be read counts as unblocked (the decision itself never depends on this).
async fn present(state: &AppState, rows: Vec<PendingRow>) -> Result<Vec<PendingRequest>, ApiError> {
    let signals = state.company_signals.as_ref();
    let wanted = rows
        .iter()
        .any(|r| r.state == PendingState::Requested && local_category(&r.host, signals).is_some());
    let blocked: HashMap<PendingId, LocalCategory> = if wanted {
        let _lock = state.settings_lock.lock().await;
        let settings = state.settings.clone();
        let candidates: Vec<(PendingId, WorkspaceId, LocalCategory)> = rows
            .iter()
            .filter(|r| r.state == PendingState::Requested)
            .filter_map(|r| Some((r.id, r.workspace.clone(), local_category(&r.host, signals)?)))
            .collect();
        blocking(move || {
            let repo = settings.as_ref();
            let Ok(global) = load_global(repo) else {
                tracing::warn!("global settings unreadable; no pending request is marked blocked");
                return Ok(HashMap::new());
            };
            let mut per_workspace = HashMap::new();
            let mut blocked = HashMap::new();
            for (id, workspace, category) in candidates {
                let loaded = per_workspace
                    .entry(workspace.clone())
                    .or_insert_with(|| load_workspace(repo, &workspace).ok());
                let Some(loaded) = loaded else { continue };
                let effective = resolve(&global.settings, Some(&loaded.settings));
                if !effective.local_toggles.get(category).value {
                    blocked.insert(id, category);
                }
            }
            Ok(blocked)
        })
        .await?
    } else {
        HashMap::new()
    };
    rows.into_iter()
        .map(|row| {
            let id = row.id;
            let host = row.host.clone();
            let mut request = PendingRequest::from_store(row)?;
            request.blocked_by = blocked.get(&id).copied();
            if request.state == crate::wire::PendingState::Requested {
                request.company_signal = company_signal(&host, signals).map(str::to_owned);
            }
            Ok(request)
        })
        .collect()
}

/// Open (`requested`) requests, most recent first.
#[utoipa::path(
    get,
    path = "/api/pending",
    tag = "pending",
    params(("workspace" = Option<WorkspaceId>, Query, description = "only this workspace's requests")),
    responses(
        (status = OK, description = "open requests", body = PendingList),
        (status = BAD_REQUEST, description = "invalid workspace name", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_pending(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<PendingQuery>,
) -> Result<Json<PendingList>, ApiError> {
    let store = state.store.clone();
    let rows =
        blocking(move || Ok(store.open_pending(principal.user, query.workspace.as_ref())?)).await?;
    Ok(Json(PendingList {
        requests: present(&state, rows).await?,
    }))
}

/// Open requests grouped by registrable domain, most recent group first (R-18).
#[utoipa::path(
    get,
    path = "/api/inbox",
    tag = "pending",
    responses((status = OK, description = "the inbox", body = Inbox))
)]
pub(crate) async fn inbox(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Inbox>, ApiError> {
    let store = state.store.clone();
    let groups = blocking(move || Ok(store.inbox(principal.user)?)).await?;
    let mut out = Vec::with_capacity(groups.len());
    for group in groups {
        out.push(InboxGroup {
            registrable_domain: group.registrable_domain,
            requests: present(&state, group.rows).await?,
        });
    }
    Ok(Json(Inbox {
        groups: out,
        open_limit: state.store.max_open_rows(),
    }))
}

/// One pending request, in any state.
#[utoipa::path(
    get,
    path = "/api/pending/{id}",
    tag = "pending",
    params(("id" = i64, Path, description = "pending request id")),
    responses(
        (status = OK, description = "the request", body = PendingRequest),
        (status = NOT_FOUND, description = "no such request", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_pending(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<PendingRequest>, ApiError> {
    let store = state.store.clone();
    let row = blocking(move || Ok(store.pending(PendingId(id))?)).await?;
    Ok(Json(present(&state, vec![row]).await?.remove(0)))
}

/// Approves an open request: creates an allow rule (this workspace, exact host, permanent unless
/// the body says otherwise) and closes the other open requests it now decides.
#[utoipa::path(
    post,
    path = "/api/pending/{id}/approve",
    tag = "pending",
    params(("id" = i64, Path, description = "pending request id")),
    request_body = DecisionRequest,
    responses(
        (status = OK, description = "approved", body = DecisionOutcome),
        (status = NOT_FOUND, description = "no such request", body = ApiErrorBody),
        (status = CONFLICT, description = "already decided or expired", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "invalid suffix or expiry", body = ApiErrorBody)
    )
)]
pub(crate) async fn approve(
    state: State<AppState>,
    principal: Principal,
    id: Path<i64>,
    body: crate::extract::Json<DecisionRequest>,
) -> Result<Json<DecisionOutcome>, ApiError> {
    decide(state, principal, id, body, Effect::Allow).await
}

/// Denies an open request: creates a deny rule (this workspace, exact host, permanent unless the
/// body says otherwise) and closes the other open requests it now decides.
#[utoipa::path(
    post,
    path = "/api/pending/{id}/deny",
    tag = "pending",
    params(("id" = i64, Path, description = "pending request id")),
    request_body = DecisionRequest,
    responses(
        (status = OK, description = "denied", body = DecisionOutcome),
        (status = NOT_FOUND, description = "no such request", body = ApiErrorBody),
        (status = CONFLICT, description = "already decided or expired", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "invalid suffix or expiry", body = ApiErrorBody)
    )
)]
pub(crate) async fn deny(
    state: State<AppState>,
    principal: Principal,
    id: Path<i64>,
    body: crate::extract::Json<DecisionRequest>,
) -> Result<Json<DecisionOutcome>, ApiError> {
    decide(state, principal, id, body, Effect::Deny).await
}

async fn decide(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<i64>,
    crate::extract::Json(body): crate::extract::Json<DecisionRequest>,
    effect: Effect,
) -> Result<Json<DecisionOutcome>, ApiError> {
    let resolution = body.into_resolution(effect)?;
    // The request is the principal's (checked on the route). A rule beyond its workspace is a
    // global rule or a set entry of the workspace's user, which the store makes it; changing those
    // is an action of its own.
    if resolution.scope != ScopeChoice::Workspace {
        authz::check(
            &state,
            principal,
            Action::ChangeGlobalRules,
            ResourceRef::Own,
        )
        .await?;
    }
    let decided = blocking(move || {
        Ok(state
            .store
            .resolve_pending(PendingId(id), &resolution, Actor::Api)?)
    })
    .await?;
    Ok(Json(DecisionOutcome::from_store(decided)?))
}
