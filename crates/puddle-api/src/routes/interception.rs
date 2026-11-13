// SPDX-License-Identifier: GPL-3.0-or-later
//! Decrypting every host: a workspace's switch and the names that are never decrypted.
//!
//! The settings live in the database. A change reaches a running workspace at once: the store
//! announces it and the host recomputes what the workspace decrypts, with no restart.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_netpolicy::normalise_host;
use puddle_store::InterceptionSettings;
use puddle_types::{CURATED_PASS_THROUGH, Host, InterceptionMode, WorkspaceId};

use crate::ApiErrorBody;
use crate::authz::{self, Action, Principal, ResourceRef};
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::wire::{
    GlobalNeverDecrypt, InterceptionRequest, InterceptionSettingsRequest, InterceptionSettingsView,
    InterceptionView, LearnedHosts, LearnedLimits, MakePermanentRequest, PermanentScope,
    WorkspaceMode,
};

/// Most names one list may hold.
const MAX_NAMES: usize = 500;

/// A never-decrypt name in its stored form: lower case, no trailing dot, `*.` kept. The same
/// rules as the proxy's pattern set, so every stored name is one the proxy accepts.
pub(super) fn pattern(raw: &str) -> Result<String, ApiError> {
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

async fn named(state: &AppState, raw: &str) -> Result<WorkspaceId, ApiError> {
    let id =
        WorkspaceId::new(raw).map_err(|_| ApiError::not_found(format!("no workspace {raw:?}")))?;
    Ok(state.workspaces.get(&id).await?.id)
}

fn settings_view(state: &AppState) -> Result<InterceptionSettingsView, ApiError> {
    let saved = state.store.interception_settings()?;
    let mut curated: Vec<String> = CURATED_PASS_THROUGH
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    curated.sort();
    Ok(InterceptionSettingsView {
        mode: saved.effective_mode(),
        chosen: saved.mode.is_some(),
        learn_from_rejections: saved.learn_from_rejections,
        notice_seen: saved.notice_seen,
        curated,
    })
}

fn view(state: &AppState, name: &WorkspaceId) -> Result<InterceptionView, ApiError> {
    let own = state.store.workspace_interception(name)?;
    let effective_mode = own.mode.unwrap_or_else(|| {
        state.store.interception_settings().map_or(
            InterceptionMode::EXISTING_INSTALL,
            InterceptionSettings::effective_mode,
        )
    });
    Ok(InterceptionView {
        workspace: name.clone(),
        mode: WorkspaceMode::of(own.mode),
        effective_mode,
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

/// Changes the mode and/or the workspace's never-decrypt names. A running workspace applies it
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
        if let Some(choice) = body.mode {
            state.store.set_workspace_mode(&name, choice.own())?;
        }
        if let Some(never) = never {
            state.store.set_never_decrypt(&name, &never)?;
        }
        view(&state, &name)
    })
    .await
    .map(Json)
}

/// The global interception setting: the mode workspaces follow, whether the user has chosen one,
/// learning from rejections, and the curated list.
#[utoipa::path(
    get,
    path = "/api/interception",
    tag = "interception",
    responses((status = OK, description = "the global setting", body = InterceptionSettingsView))
)]
pub(crate) async fn get_interception_settings(
    State(state): State<AppState>,
) -> Result<Json<InterceptionSettingsView>, ApiError> {
    Ok(Json(blocking(move || settings_view(&state)).await?))
}

/// Changes the global setting. A running workspace that follows it applies the change at once.
#[utoipa::path(
    put,
    path = "/api/interception",
    tag = "interception",
    request_body = InterceptionSettingsRequest,
    responses((status = OK, description = "saved; the setting now", body = InterceptionSettingsView))
)]
pub(crate) async fn put_interception_settings(
    State(state): State<AppState>,
    crate::extract::Json(body): crate::extract::Json<InterceptionSettingsRequest>,
) -> Result<Json<InterceptionSettingsView>, ApiError> {
    blocking(move || {
        let mut saved = state.store.interception_settings()?;
        if let Some(mode) = body.mode {
            saved.mode = Some(mode);
            // Choosing answers the notice.
            saved.notice_seen = true;
        }
        if let Some(on) = body.learn_from_rejections {
            saved.learn_from_rejections = on;
        }
        if let Some(seen) = body.notice_seen {
            saved.notice_seen = seen;
        }
        state.store.set_interception_settings(saved)?;
        settings_view(&state)
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

/// Shortest time a host may be passed through: a minute.
const MIN_TTL_SECS: u32 = 60;
/// Longest: 30 days.
const MAX_TTL_SECS: u32 = 30 * 24 * 3600;
/// Most hosts a workspace may remember.
const MAX_CAP: u32 = 10_000;

/// The hosts the workspace's tools refused Puddle's certificate for, with when each was first
/// and last refused and how many times, and whether Puddle passes it through now. A workspace
/// that is not running passes nothing through. A client refetches on the
/// `pass_through_learned` event.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/interception/learned",
    tag = "interception",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the learned hosts", body = LearnedHosts),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_learned(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<LearnedHosts>, ApiError> {
    let name = named(&state, &id).await?;
    Ok(Json(LearnedHosts {
        hosts: state.learned.list(&name),
    }))
}

/// Decrypts every host the workspace passes through again from the next connection. Connections
/// already open keep going as they are; the history of refusals stays.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/interception/learned",
    tag = "interception",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = NO_CONTENT, description = "the list is empty now"),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn clear_learned(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let name = named(&state, &id).await?;
    state.learned.clear(&name);
    Ok(StatusCode::NO_CONTENT)
}

/// Decrypts one learned host again from the next connection; its history stays. If the tool
/// still refuses Puddle's certificate, the host is learned again, and the user is told again.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/interception/learned/{host}",
    tag = "interception",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("host" = String, Path, description = "the learned host name")
    ),
    responses(
        (status = NO_CONTENT, description = "the host is decrypted again"),
        (status = NOT_FOUND, description = "no such workspace, or the host is not passed through", body = ApiErrorBody)
    )
)]
pub(crate) async fn forget_learned(
    State(state): State<AppState>,
    Path((id, host)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let name = named(&state, &id).await?;
    let learned = match normalise_host(&host).map(puddle_netpolicy::Target::into_host) {
        Ok(Host::Name(host)) => host.to_string(),
        _ => String::new(),
    };
    if !learned.is_empty() && state.learned.forget(&name, &learned) {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!(
            "{host:?} is not passed through in {name}"
        )))
    }
}

/// Never decrypts a learned host again, in this workspace or in every workspace: the host joins
/// that never-decrypt list and leaves the learned list, history included. Every workspace's list
/// is the install's, so `global` is for the owner alone, as `PUT /api/interception/never-decrypt`
/// is.
#[utoipa::path(
    post,
    path = "/api/workspaces/{id}/interception/learned/{host}/permanent",
    tag = "interception",
    params(
        ("id" = String, Path, description = "workspace id"),
        ("host" = String, Path, description = "the learned host name")
    ),
    request_body = MakePermanentRequest,
    responses(
        (status = NO_CONTENT, description = "the host is on the never-decrypt list and off the learned list"),
        (status = FORBIDDEN, description = "`global` and the caller is not the owner", body = ApiErrorBody),
        (status = NOT_FOUND, description = "no such workspace, or the host is not on the learned list", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the never-decrypt list is full", body = ApiErrorBody)
    )
)]
pub(crate) async fn make_permanent(
    State(state): State<AppState>,
    principal: Principal,
    Path((id, host)): Path<(String, String)>,
    crate::extract::Json(body): crate::extract::Json<MakePermanentRequest>,
) -> Result<StatusCode, ApiError> {
    if body.scope == PermanentScope::Global {
        authz::check(
            &state,
            principal,
            Action::ChangeInstall,
            ResourceRef::Install,
        )
        .await?;
    }
    let name = named(&state, &id).await?;
    let learned = match normalise_host(&host).map(puddle_netpolicy::Target::into_host) {
        Ok(Host::Name(host)) => host.to_string(),
        _ => String::new(),
    };
    if learned.is_empty() || !state.learned.list(&name).iter().any(|h| h.host == learned) {
        return Err(ApiError::not_found(format!(
            "{host:?} is not on the learned list of {name}"
        )));
    }
    blocking({
        let (state, name, learned) = (state.clone(), name.clone(), learned.clone());
        move || {
            match body.scope {
                PermanentScope::Workspace => {
                    let mut names = state.store.workspace_interception(&name)?.never_decrypt;
                    names.push(learned);
                    state.store.set_never_decrypt(&name, &patterns(&names)?)?;
                }
                PermanentScope::Global => {
                    let mut names = state.store.global_never_decrypt()?;
                    names.push(learned);
                    state.store.set_global_never_decrypt(&patterns(&names)?)?;
                }
            }
            Ok(())
        }
    })
    .await?;
    state.learned.discard(&name, &learned);
    Ok(StatusCode::NO_CONTENT)
}

/// How long and how many hosts Puddle passes through after a tool refused its certificate.
#[utoipa::path(
    get,
    path = "/api/interception/learned-limits",
    tag = "interception",
    responses((status = OK, description = "the limits (12 hours and 1024 hosts until changed)", body = LearnedLimits))
)]
pub(crate) async fn get_learned_limits(
    State(state): State<AppState>,
) -> Result<Json<LearnedLimits>, ApiError> {
    Ok(Json(blocking(move || limits(&state)).await?))
}

fn limits(state: &AppState) -> Result<LearnedLimits, ApiError> {
    let saved = state.store.learned_limits()?;
    Ok(LearnedLimits {
        ttl_secs: saved.ttl_secs,
        cap: saved.cap,
    })
}

/// Changes how long and how many hosts a workspace passes through after a tool refused Puddle's
/// certificate. Running workspaces that decrypt every host apply it at once: a longer time or
/// larger list keeps what is learned, a shorter or smaller one drops what no longer fits.
#[utoipa::path(
    put,
    path = "/api/interception/learned-limits",
    tag = "interception",
    request_body = LearnedLimits,
    responses(
        (status = OK, description = "saved; the limits now", body = LearnedLimits),
        (status = UNPROCESSABLE_ENTITY, description = "the time is not 60 to 2 592 000 seconds, or the size is not 1 to 10 000", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_learned_limits(
    State(state): State<AppState>,
    crate::extract::Json(body): crate::extract::Json<LearnedLimits>,
) -> Result<Json<LearnedLimits>, ApiError> {
    if !(MIN_TTL_SECS..=MAX_TTL_SECS).contains(&body.ttl_secs) {
        return Err(ApiError::invalid(format!(
            "the time must be {MIN_TTL_SECS} to {MAX_TTL_SECS} seconds ({} given)",
            body.ttl_secs
        )));
    }
    if !(1..=MAX_CAP).contains(&body.cap) {
        return Err(ApiError::invalid(format!(
            "the size must be 1 to {MAX_CAP} hosts ({} given)",
            body.cap
        )));
    }
    blocking(move || {
        state
            .store
            .set_learned_limits(puddle_store::LearnedLimits {
                ttl_secs: body.ttl_secs,
                cap: body.cap,
            })?;
        limits(&state)
    })
    .await
    .map(Json)
}
