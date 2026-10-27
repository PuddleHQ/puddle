// SPDX-License-Identifier: GPL-3.0-or-later
//! Decrypting every host: a workspace's switch and the names that are never decrypted.
//!
//! The settings live in the database. A change reaches a running workspace at once: the store
//! announces it and the host recomputes what the workspace decrypts, with no restart.

use axum::Json;
use axum::extract::State;
use puddle_netpolicy::normalise_host;
use puddle_types::{Host, WorkspaceName};

use crate::ApiErrorBody;
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::wire::{GlobalNeverDecrypt, InterceptionRequest, InterceptionView};

/// Most names one list may hold.
const MAX_NAMES: usize = 500;

/// A never-decrypt name in its stored form: lower case, no trailing dot, `*.` kept. The same
/// rules as the proxy's pattern set, so every stored name is one the proxy accepts.
fn pattern(raw: &str) -> Result<String, ApiError> {
    let (wild, name) = raw
        .trim()
        .strip_prefix("*.")
        .map_or((false, raw.trim()), |rest| (true, rest));
    let host = normalise_host(name)
        .map_err(|_| ApiError::invalid(format!("{raw:?} is not a host name")))?
        .into_host();
    let Host::Name(name) = host else {
        return Err(ApiError::invalid(format!(
            "{raw:?} is an IP address; only names can be listed"
        )));
    };
    let name = name.to_string();
    if wild {
        if name.split('.').count() < 2 {
            return Err(ApiError::invalid(format!(
                "{raw:?} is too wide: a pattern needs at least two labels after `*.`"
            )));
        }
        Ok(format!("*.{name}"))
    } else {
        Ok(name)
    }
}

fn patterns(raw: &[String]) -> Result<Vec<String>, ApiError> {
    if raw.len() > MAX_NAMES {
        return Err(ApiError::invalid(format!(
            "at most {MAX_NAMES} names ({} given)",
            raw.len()
        )));
    }
    let mut out = raw
        .iter()
        .map(|p| pattern(p))
        .collect::<Result<Vec<_>, _>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

async fn named(state: &AppState, raw: &str) -> Result<WorkspaceName, ApiError> {
    let id = puddle_types::WorkspaceId::new(raw)
        .map_err(|_| ApiError::not_found(format!("no workspace {raw:?}")))?;
    Ok(state.workspaces.get(&id).await?.name)
}

fn view(state: &AppState, name: &WorkspaceName) -> Result<InterceptionView, ApiError> {
    let own = state.store.workspace_interception(name)?;
    Ok(InterceptionView {
        workspace: name.clone(),
        decrypt_all: own.decrypt_all,
        never_decrypt: own.never_decrypt,
        global_never_decrypt: state.store.global_never_decrypt()?,
    })
}

/// How a workspace decrypts hosts: the switch and the never-decrypt names (its own and the
/// global ones).
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/interception",
    tag = "interception",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the workspace's interception settings", body = InterceptionView),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_interception(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<InterceptionView>, ApiError> {
    let name = named(&state, &id).await?;
    Ok(Json(blocking(move || view(&state, &name)).await?))
}

/// Changes the switch and/or the workspace's never-decrypt names. A running workspace applies it
/// at once: the next connection to a host is decrypted or passed through accordingly. A host with
/// a credential, secret or login capture is always decrypted, whatever the list says.
#[utoipa::path(
    put,
    path = "/api/workspaces/{id}/interception",
    tag = "interception",
    params(("id" = String, Path, description = "workspace id")),
    request_body = InterceptionRequest,
    responses(
        (status = OK, description = "saved; the settings now", body = InterceptionView),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a name is not a host name, is an IP address or is too wide, or there are too many", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_interception(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<InterceptionRequest>,
) -> Result<Json<InterceptionView>, ApiError> {
    let name = named(&state, &id).await?;
    let never = body.never_decrypt.as_deref().map(patterns).transpose()?;
    blocking(move || {
        if let Some(on) = body.decrypt_all {
            state.store.set_decrypt_all(&name, on)?;
        }
        if let Some(never) = never {
            state.store.set_never_decrypt(&name, &never)?;
        }
        view(&state, &name)
    })
    .await
    .map(Json)
}

/// The names no workspace decrypts.
#[utoipa::path(
    get,
    path = "/api/interception/never-decrypt",
    tag = "interception",
    responses((status = OK, description = "the global never-decrypt names", body = GlobalNeverDecrypt))
)]
pub(crate) async fn get_global_never_decrypt(
    State(state): State<AppState>,
) -> Result<Json<GlobalNeverDecrypt>, ApiError> {
    let never_decrypt = blocking(move || Ok(state.store.global_never_decrypt()?)).await?;
    Ok(Json(GlobalNeverDecrypt { never_decrypt }))
}

/// Replaces the names no workspace decrypts. Running workspaces that decrypt every host apply it
/// at once.
#[utoipa::path(
    put,
    path = "/api/interception/never-decrypt",
    tag = "interception",
    request_body = GlobalNeverDecrypt,
    responses(
        (status = OK, description = "saved; the list now", body = GlobalNeverDecrypt),
        (status = UNPROCESSABLE_ENTITY, description = "a name is not a host name, is an IP address or is too wide, or there are too many", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_global_never_decrypt(
    State(state): State<AppState>,
    crate::extract::Json(body): crate::extract::Json<GlobalNeverDecrypt>,
) -> Result<Json<GlobalNeverDecrypt>, ApiError> {
    let never = patterns(&body.never_decrypt)?;
    let never_decrypt = blocking(move || {
        state.store.set_global_never_decrypt(&never)?;
        Ok(state.store.global_never_decrypt()?)
    })
    .await?;
    Ok(Json(GlobalNeverDecrypt { never_decrypt }))
}
