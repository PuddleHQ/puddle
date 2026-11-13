// SPDX-License-Identifier: AGPL-3.0-or-later
//! The proxy sign-ins: a user name and password for a company proxy that asks for Basic. The
//! password goes in and never comes out; the list names proxies and users only.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_upstream::ProxyAddr;

use crate::ApiErrorBody;
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::wire::{ProxySignInList, ProxySignInRequest};

fn proxy_addr(text: &str) -> Result<ProxyAddr, ApiError> {
    ProxyAddr::parse(text).map_err(|_| {
        ApiError::invalid("the proxy must be a host name and port such as proxy.example.com:8080")
    })
}

/// The proxies Puddle holds a sign-in for, with the user name of each. Never a password.
#[utoipa::path(
    get,
    path = "/api/proxy-sign-ins",
    tag = "network",
    responses((status = OK, description = "the stored sign-ins", body = ProxySignInList))
)]
pub(crate) async fn list_proxy_sign_ins(
    State(state): State<AppState>,
) -> Result<Json<ProxySignInList>, ApiError> {
    let sign_ins = blocking(move || Ok(state.proxy_sign_ins.list())).await?;
    Ok(Json(ProxySignInList { sign_ins }))
}

/// Stores a user name and password for a proxy, replacing an earlier one. The password is kept in
/// the operating system's credential store under Puddle's own name; the next connection that
/// asks for Basic uses it, after the Windows sign-in.
#[utoipa::path(
    put,
    path = "/api/proxy-sign-ins",
    tag = "network",
    request_body = ProxySignInRequest,
    responses(
        (status = OK, description = "stored; the sign-ins now", body = ProxySignInList),
        (status = UNPROCESSABLE_ENTITY, description = "the proxy, user name or password is not usable", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the credential store did not answer, or this build has no proxy sign-in", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_proxy_sign_in(
    State(state): State<AppState>,
    crate::extract::Json(body): crate::extract::Json<ProxySignInRequest>,
) -> Result<Json<ProxySignInList>, ApiError> {
    let proxy = proxy_addr(&body.proxy)?;
    blocking(move || {
        state
            .proxy_sign_ins
            .set(&proxy, &body.user, body.password.0.expose())?;
        Ok(ProxySignInList {
            sign_ins: state.proxy_sign_ins.list(),
        })
    })
    .await
    .map(Json)
}

/// Forgets the sign-in for a proxy. One that is not stored is fine.
#[utoipa::path(
    delete,
    path = "/api/proxy-sign-ins/{proxy}",
    tag = "network",
    params(("proxy" = String, Path, description = "the proxy as host:port")),
    responses(
        (status = NO_CONTENT, description = "no sign-in is stored for the proxy now"),
        (status = UNPROCESSABLE_ENTITY, description = "not a proxy address", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the credential store did not answer", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_proxy_sign_in(
    State(state): State<AppState>,
    Path(proxy): Path<String>,
) -> Result<StatusCode, ApiError> {
    let proxy = proxy_addr(&proxy)?;
    blocking(move || Ok(state.proxy_sign_ins.remove(&proxy)?)).await?;
    Ok(StatusCode::NO_CONTENT)
}
