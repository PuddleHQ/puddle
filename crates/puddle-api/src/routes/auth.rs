// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser sign-in: the first visitor, passwords, the session cookie, devices and the sign-in
//! methods. See [`crate::signin`] for how a session works.
//!
//! The routes that run before there is a token (status, setup, sign-in, invite, refresh,
//! sign-out) answer only a request that came in under a public name; on the loopback address,
//! where the desktop window and the command line live, they do not exist.

use axum::Json;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use puddle_store::DeviceId;

use crate::ApiErrorBody;
use crate::auth::ReachedPublicly;
use crate::authz::{Principal, Via};
use crate::error::{ApiError, ErrorCode};
use crate::extract::SecretJson;
use crate::routes::AppState;
use crate::signin::{COOKIE_NAME, DEVICE_TTL_MS, Granted, SignIn};
use crate::wire::{
    AuthStatus, ChangePasswordRequest, DeviceList, InviteRequest, PasswordSignInRequest,
    SessionGrant, SetupRequest, SignInMethodRequest, SignInMethods,
};

/// A request that came in under a public name.
pub(crate) struct Public;

impl<S: Send + Sync> FromRequestParts<S> for Public {
    type Rejection = ApiError;

    fn from_request_parts(
        parts: &mut Parts,
        _state: &S,
    ) -> impl Future<Output = Result<Self, ApiError>> + Send {
        std::future::ready(
            parts
                .extensions
                .get::<ReachedPublicly>()
                .map(|_| Public)
                .ok_or_else(|| ApiError::not_found("no such route")),
        )
    }
}

fn service(state: &AppState) -> Result<&std::sync::Arc<SignIn>, ApiError> {
    state.sign_in.as_ref().ok_or_else(|| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::Unavailable,
            "signing in is not available here",
        )
    })
}

const NO_STORE: (HeaderName, HeaderValue) =
    (header::CACHE_CONTROL, HeaderValue::from_static("no-store"));

/// The refresh cookie: `__Host-` (so it is bound to this exact host, over TLS, from the root
/// path), `HttpOnly` (the page's scripts cannot read it) and `SameSite=Strict`.
fn session_cookie(refresh: &str) -> HeaderValue {
    let max_age = DEVICE_TTL_MS / 1000;
    cookie_value(&format!(
        "{COOKIE_NAME}={refresh}; Path=/; Max-Age={max_age}; Secure; HttpOnly; SameSite=Strict"
    ))
}

fn cleared_cookie() -> HeaderValue {
    cookie_value(&format!(
        "{COOKIE_NAME}=; Path=/; Max-Age=0; Secure; HttpOnly; SameSite=Strict"
    ))
}

fn cookie_value(text: &str) -> HeaderValue {
    // Base64url text and fixed words only: always a valid header value.
    HeaderValue::from_str(text).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// The refresh token in the request's cookies, if exactly one cookie has the name.
fn refresh_cookie(headers: &HeaderMap) -> Option<String> {
    let mut found = None;
    for value in headers.get_all(header::COOKIE) {
        for pair in value.to_str().ok()?.split(';') {
            if let Some((name, value)) = pair.trim().split_once('=')
                && name == COOKIE_NAME
            {
                if found.is_some() {
                    return None;
                }
                found = Some(value.to_owned());
            }
        }
    }
    found.filter(|v| !v.is_empty())
}

fn granted(status: StatusCode, granted: Granted) -> Response {
    let Granted { grant, refresh } = granted;
    (
        status,
        [NO_STORE, (header::SET_COOKIE, session_cookie(&refresh))],
        Json(grant),
    )
        .into_response()
}

/// Whether the owner account has to be made, and whether a password signs in. Public: it names
/// no one.
#[utoipa::path(
    get,
    path = "/api/auth/status",
    tag = "auth",
    security(()),
    responses(
        (status = OK, description = "what the sign-in page shows", body = AuthStatus),
        (status = SERVICE_UNAVAILABLE, description = "signing in is not available", body = ApiErrorBody)
    )
)]
pub(crate) async fn status(
    State(state): State<AppState>,
    _public: Public,
) -> Result<Json<AuthStatus>, ApiError> {
    Ok(Json(service(&state)?.status().await?))
}

/// The first visitor makes the owner account and is signed in as it. Works once: after that it
/// answers 409, and resetting the owner's password never reopens it.
#[utoipa::path(
    post,
    path = "/api/auth/setup",
    tag = "auth",
    security(()),
    request_body = SetupRequest,
    responses(
        (status = CREATED, description = "the owner account exists and this browser is signed in; the refresh token is in the `Set-Cookie` header", body = SessionGrant),
        (status = CONFLICT, description = "the owner account is already made", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the name or password was refused", body = ApiErrorBody),
        (status = TOO_MANY_REQUESTS, description = "too many attempts: see `Retry-After`", body = ApiErrorBody)
    )
)]
pub(crate) async fn setup(
    State(state): State<AppState>,
    _public: Public,
    SecretJson(request): SecretJson<SetupRequest>,
) -> Result<Response, ApiError> {
    let granted_session = service(&state)?
        .setup(
            &request.name,
            request.password.0.expose(),
            request.device_name.as_deref(),
        )
        .await?;
    Ok(granted(StatusCode::CREATED, granted_session))
}

/// Signs in with a name and a password. A wrong name and a wrong password answer the same, and
/// repeated failures lock the account for a while (429 with `Retry-After`).
#[utoipa::path(
    post,
    path = "/api/auth/sign-in",
    tag = "auth",
    security(()),
    request_body = PasswordSignInRequest,
    responses(
        (status = OK, description = "signed in; the refresh token is in the `Set-Cookie` header", body = SessionGrant),
        (status = UNAUTHORIZED, description = "wrong name or password", body = ApiErrorBody),
        (status = TOO_MANY_REQUESTS, description = "too many attempts: see `Retry-After`", body = ApiErrorBody)
    )
)]
pub(crate) async fn password_sign_in(
    State(state): State<AppState>,
    _public: Public,
    SecretJson(request): SecretJson<PasswordSignInRequest>,
) -> Result<Response, ApiError> {
    let granted_session = service(&state)?
        .sign_in(
            &request.name,
            request.password.0.expose(),
            request.device_name.as_deref(),
        )
        .await?;
    Ok(granted(StatusCode::OK, granted_session))
}

/// Sets a password with the secret of an invite link the server's reset command made, and signs in.
/// A link works once.
#[utoipa::path(
    post,
    path = "/api/auth/invite",
    tag = "auth",
    security(()),
    request_body = InviteRequest,
    responses(
        (status = OK, description = "the password is set and this browser is signed in", body = SessionGrant),
        (status = UNAUTHORIZED, description = "the link is not valid any more", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the password was refused", body = ApiErrorBody),
        (status = TOO_MANY_REQUESTS, description = "too many attempts: see `Retry-After`", body = ApiErrorBody)
    )
)]
pub(crate) async fn invite(
    State(state): State<AppState>,
    _public: Public,
    SecretJson(request): SecretJson<InviteRequest>,
) -> Result<Response, ApiError> {
    let granted_session = service(&state)?
        .redeem_invite(
            request.secret.0.expose(),
            request.password.0.expose(),
            request.device_name.as_deref(),
        )
        .await?;
    Ok(granted(StatusCode::OK, granted_session))
}

/// A new access token, for the browser whose session cookie comes with the request. The request
/// must name the page's own `Origin`.
#[utoipa::path(
    post,
    path = "/api/auth/refresh",
    tag = "auth",
    security(()),
    responses(
        (status = OK, description = "a new access token for 15 minutes", body = SessionGrant),
        (status = UNAUTHORIZED, description = "this browser is not signed in (any more)", body = ApiErrorBody)
    )
)]
pub(crate) async fn refresh(
    State(state): State<AppState>,
    _public: Public,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let service = service(&state)?;
    let session = match refresh_cookie(&headers) {
        Some(cookie) => service.refresh(&cookie).await,
        None => Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            ErrorCode::Unauthorized,
            "this browser is not signed in",
        )),
    };
    match session {
        Ok(grant) => Ok((StatusCode::OK, [NO_STORE], Json(grant)).into_response()),
        Err(err) => {
            // A dead cookie is dropped, so the page stops sending it.
            let mut response = err.into_response();
            response
                .headers_mut()
                .insert(header::SET_COOKIE, cleared_cookie());
            Ok(response)
        }
    }
}

/// Signs this browser out: its device is revoked, its tokens end, its cookie is cleared. Signing
/// out a browser that is not signed in is fine.
#[utoipa::path(
    post,
    path = "/api/auth/sign-out",
    tag = "auth",
    security(()),
    responses((status = NO_CONTENT, description = "signed out"))
)]
pub(crate) async fn sign_out(
    State(state): State<AppState>,
    _public: Public,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(cookie) = refresh_cookie(&headers) {
        service(&state)?.sign_out(&cookie).await?;
    }
    Ok((
        StatusCode::NO_CONTENT,
        [(header::SET_COOKIE, cleared_cookie())],
    )
        .into_response())
}

/// Changes the signed-in user's password; every other device is signed out. Only a signed-in
/// browser can: the desktop window and the command line have no password to change.
#[utoipa::path(
    put,
    path = "/api/auth/password",
    tag = "auth",
    request_body = ChangePasswordRequest,
    responses(
        (status = NO_CONTENT, description = "changed"),
        (status = UNAUTHORIZED, description = "the current password is wrong", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the new password was refused", body = ApiErrorBody),
        (status = TOO_MANY_REQUESTS, description = "too many attempts: see `Retry-After`", body = ApiErrorBody)
    )
)]
pub(crate) async fn change_password(
    State(state): State<AppState>,
    principal: Principal,
    SecretJson(request): SecretJson<ChangePasswordRequest>,
) -> Result<StatusCode, ApiError> {
    let Via::Device(device) = principal.via else {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            ErrorCode::Forbidden,
            "only a signed-in browser has a password to change",
        ));
    };
    service(&state)?
        .change_password(
            principal.user,
            device,
            request.current.0.expose(),
            request.new.0.expose(),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The signed-in user's devices (browsers that are signed in), the one used last first.
#[utoipa::path(
    get,
    path = "/api/devices",
    tag = "auth",
    responses((status = OK, description = "the devices", body = DeviceList))
)]
pub(crate) async fn list_devices(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<DeviceList>, ApiError> {
    let current = match principal.via {
        Via::Device(id) => Some(id),
        Via::Install => None,
    };
    Ok(Json(
        service(&state)?.devices(principal.user, current).await?,
    ))
}

/// Signs a device out: its access tokens, its refresh token and its event streams end. A device
/// that is not the caller's answers as one that does not exist.
#[utoipa::path(
    delete,
    path = "/api/devices/{id}",
    tag = "auth",
    params(("id" = i64, Path, description = "the device")),
    responses(
        (status = NO_CONTENT, description = "revoked"),
        (status = NOT_FOUND, description = "no such device", body = ApiErrorBody)
    )
)]
pub(crate) async fn revoke_device(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<i64>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .revoke_device(principal.user, DeviceId::from_stored(id))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Which ways of signing in exist, and whether each is on.
#[utoipa::path(
    get,
    path = "/api/auth/methods",
    tag = "auth",
    responses((status = OK, description = "the methods", body = SignInMethods))
)]
pub(crate) async fn get_methods(
    State(state): State<AppState>,
) -> Result<Json<SignInMethods>, ApiError> {
    Ok(Json(service(&state)?.methods().await?))
}

/// Switches a way of signing in on or off (the owner). The last one that is on can't be switched
/// off.
#[utoipa::path(
    put,
    path = "/api/auth/methods",
    tag = "auth",
    request_body = SignInMethodRequest,
    responses(
        (status = OK, description = "the methods now", body = SignInMethods),
        (status = UNPROCESSABLE_ENTITY, description = "that would leave no way to sign in, or the method does not exist yet", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_methods(
    State(state): State<AppState>,
    crate::extract::Json(request): crate::extract::Json<SignInMethodRequest>,
) -> Result<Json<SignInMethods>, ApiError> {
    Ok(Json(
        service(&state)?
            .set_method(request.method.into(), request.enabled)
            .await?,
    ))
}
