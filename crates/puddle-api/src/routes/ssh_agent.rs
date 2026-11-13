// SPDX-License-Identifier: GPL-3.0-or-later
//! The filtered SSH agent: the hosts a workspace may use the user's SSH agent for.
//!
//! Off for every host until the user turns it on for one here; no rule, default or other route
//! does. A change reaches a running workspace at once: the store announces it and the host gives
//! the workspace's proxy the new hosts, so the next request on the agent follows it, including
//! one on a connection that was already bound to a host.
//!
//! Puddle ends SSH to a Git host itself (Git over SSH), and the agent never signs a login to a Git
//! host's published key, so turning it on for such a name has no effect there.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_netpolicy::normalise_host;
use puddle_store::MAX_SSH_AGENT_KEYS;
use puddle_types::WorkspaceId;

use crate::ApiErrorBody;
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::wire::{SshAgentHostRequest, SshAgentHostView, SshAgentView};

/// Base64 characters in a SHA-256 fingerprint (`ssh-keygen -l` leaves the padding off).
const FINGERPRINT_CHARS: usize = 43;

async fn named(state: &AppState, raw: &str) -> Result<WorkspaceId, ApiError> {
    let id =
        WorkspaceId::new(raw).map_err(|_| ApiError::not_found(format!("no workspace {raw:?}")))?;
    Ok(state.workspaces.get(&id).await?.id)
}

/// The host in its stored form: a lower-case name without a trailing dot, or an IP address.
fn host(raw: &str) -> Result<String, ApiError> {
    if raw.contains('*') {
        return Err(ApiError::invalid(format!(
            "{raw:?} is a pattern; the SSH agent is turned on for one host at a time"
        )));
    }
    normalise_host(raw.trim())
        .map(|h| h.into_host().to_string())
        .map_err(|_| ApiError::invalid(format!("{raw:?} is not a host name or an IP address")))
}

fn fingerprint(raw: &str) -> Result<String, ApiError> {
    let digest = raw.trim().strip_prefix("SHA256:").unwrap_or_default();
    let base64 = |c: char| c.is_ascii_alphanumeric() || c == '+' || c == '/';
    if digest.len() == FINGERPRINT_CHARS && digest.chars().all(base64) {
        Ok(format!("SHA256:{digest}"))
    } else {
        Err(ApiError::invalid(format!(
            "{raw:?} is not a SHA-256 key fingerprint (SHA256: and {FINGERPRINT_CHARS} base64 characters, as ssh-keygen -l prints it)"
        )))
    }
}

fn fingerprints(raw: Option<&[String]>) -> Result<Option<Vec<String>>, ApiError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Err(ApiError::invalid(
            "no keys given: leave keys out for every key, or turn the agent off for the host",
        ));
    }
    if raw.len() > MAX_SSH_AGENT_KEYS {
        return Err(ApiError::invalid(format!(
            "at most {MAX_SSH_AGENT_KEYS} keys ({} given)",
            raw.len()
        )));
    }
    raw.iter()
        .map(|k| fingerprint(k))
        .collect::<Result<_, _>>()
        .map(Some)
}

fn view(state: &AppState, workspace: WorkspaceId) -> Result<SshAgentView, ApiError> {
    let hosts = state
        .store
        .ssh_agent_hosts(&workspace)?
        .into_iter()
        .map(SshAgentHostView::from)
        .collect();
    Ok(SshAgentView { workspace, hosts })
}

/// The hosts the workspace may use the user's SSH agent for, with the keys each may use.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/ssh-agent",
    tag = "ssh-agent",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the hosts, sorted by name; none until the user turns the agent on for one", body = SshAgentView),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_ssh_agent(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<SshAgentView>, ApiError> {
    let name = named(&state, &id).await?;
    Ok(Json(blocking(move || view(&state, name)).await?))
}

/// Lets the workspace use the user's SSH agent for SSH to `host`, with every key or only the
/// listed ones, or changes the keys of a host it is on for. The workspace's `ssh` then logs in to
/// that host with those keys, and only to a session Puddle saw it open there.
#[utoipa::path(
    put,
    path = "/api/workspaces/{id}/ssh-agent/{host}",
    tag = "ssh-agent",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("host" = String, Path, description = "the host's name or IP address, as ssh in the workspace names it")
    ),
    request_body = SshAgentHostRequest,
    responses(
        (status = OK, description = "on; the host now", body = SshAgentHostView),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "not a host, a pattern, a key that is not a SHA-256 fingerprint, an empty or too long key list, or the workspace has the agent on for too many hosts", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_ssh_agent_host(
    State(state): State<AppState>,
    Path((id, raw_host)): Path<(String, String)>,
    crate::extract::Json(body): crate::extract::Json<SshAgentHostRequest>,
) -> Result<Json<SshAgentHostView>, ApiError> {
    let name = named(&state, &id).await?;
    let host = host(&raw_host)?;
    let keys = fingerprints(body.keys.as_deref())?;
    blocking(move || {
        let row = state
            .store
            .set_ssh_agent_host(&name, &host, keys.as_deref())?;
        Ok(SshAgentHostView::from(row))
    })
    .await
    .map(Json)
}

/// Turns the agent off for `host`. A login to it already going on gets no more signatures.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/ssh-agent/{host}",
    tag = "ssh-agent",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("host" = String, Path, description = "the host's name or IP address")
    ),
    responses(
        (status = NO_CONTENT, description = "off"),
        (status = NOT_FOUND, description = "no such workspace, or the agent is not on for the host", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "not a host", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_ssh_agent_host(
    State(state): State<AppState>,
    Path((id, raw_host)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let name = named(&state, &id).await?;
    let host = host(&raw_host)?;
    blocking(move || {
        if state.store.remove_ssh_agent_host(&name, &host)? {
            Ok(StatusCode::NO_CONTENT)
        } else {
            Err(ApiError::not_found(format!(
                "the SSH agent is not on for {host} in {name}"
            )))
        }
    })
    .await
}
