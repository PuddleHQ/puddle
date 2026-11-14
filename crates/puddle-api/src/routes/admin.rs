// SPDX-License-Identifier: AGPL-3.0-or-later
//! The owner's admin screen: the user list and totals, making and deleting users, reissuing an
//! invite, and the audit of those. Owner only, and only when several users are on; for anyone
//! else the routes do not exist. See [`crate::admin`] for what they can never reach.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use puddle_store::UserId;
use serde::Deserialize;

use crate::ApiErrorBody;
use crate::admin::AdminService;
use crate::authz::Principal;
use crate::error::ApiError;
use crate::routes::AppState;
use crate::wire::{AdminAuditList, AdminStats, AdminUserList, CreateUserRequest, InviteIssued};

fn service(state: &AppState) -> Result<&Arc<AdminService>, ApiError> {
    state
        .admin
        .as_ref()
        .ok_or_else(|| ApiError::not_found("no such route"))
}

/// An invite is read once and never cached.
fn no_store(status: StatusCode, invite: InviteIssued) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(invite)).into_response()
}

/// Every account with counts and sizes: name, role, state, dates, devices, and how many
/// workspaces are running or stopped and what their disks hold. Never a workspace's name,
/// contents, repositories, requests, rules or audit.
#[utoipa::path(
    get,
    path = "/api/admin/users",
    tag = "admin",
    responses(
        (status = OK, description = "the accounts", body = AdminUserList),
        (status = FORBIDDEN, description = "only the owner", body = ApiErrorBody),
        (status = NOT_FOUND, description = "several users are not on", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_users(
    State(state): State<AppState>,
) -> Result<Json<AdminUserList>, ApiError> {
    Ok(Json(service(&state)?.users().await?))
}

/// The totals of this Puddle: users, workspaces, running sandboxes, disk and memory in use.
#[utoipa::path(
    get,
    path = "/api/admin/stats",
    tag = "admin",
    responses(
        (status = OK, description = "the totals", body = AdminStats),
        (status = FORBIDDEN, description = "only the owner", body = ApiErrorBody),
        (status = NOT_FOUND, description = "several users are not on", body = ApiErrorBody)
    )
)]
pub(crate) async fn admin_stats(
    State(state): State<AppState>,
) -> Result<Json<AdminStats>, ApiError> {
    Ok(Json(service(&state)?.stats().await?))
}

/// Makes an account in the invited state and answers with its one-time invite secret, once. The
/// owner hands the link `<address>/invite#<secret>` to the person, who chooses their own password.
/// The owner never types, sees or sets a password.
#[utoipa::path(
    post,
    path = "/api/admin/users",
    tag = "admin",
    request_body = CreateUserRequest,
    responses(
        (status = CREATED, description = "the account and its invite, shown once", body = InviteIssued),
        (status = FORBIDDEN, description = "only the owner", body = ApiErrorBody),
        (status = NOT_FOUND, description = "several users are not on", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the name was refused or is taken", body = ApiErrorBody)
    )
)]
pub(crate) async fn create_user(
    State(state): State<AppState>,
    principal: Principal,
    crate::extract::Json(request): crate::extract::Json<CreateUserRequest>,
) -> Result<Response, ApiError> {
    let invite = service(&state)?
        .create(principal.user, &request.name)
        .await?;
    Ok(no_store(StatusCode::CREATED, invite))
}

/// A new invite for a user (an expired one, or a forgotten password). It ends that user's
/// password and every browser signed in as them, and invalidates the old invite. It reveals
/// nothing about the account.
#[utoipa::path(
    post,
    path = "/api/admin/users/{id}/invite",
    tag = "admin",
    params(("id" = i64, Path, description = "the user")),
    responses(
        (status = OK, description = "the new invite, shown once", body = InviteIssued),
        (status = FORBIDDEN, description = "only the owner", body = ApiErrorBody),
        (status = NOT_FOUND, description = "no such user, or several users are not on", body = ApiErrorBody),
        (status = CONFLICT, description = "the owner, or a user being deleted", body = ApiErrorBody)
    )
)]
pub(crate) async fn reissue_invite(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let invite = service(&state)?
        .reissue(principal.user, UserId::from_stored(id))
        .await?;
    Ok(no_store(StatusCode::OK, invite))
}

/// What deleting a user must confirm.
#[derive(Debug, Deserialize)]
pub(crate) struct ConfirmDelete {
    /// The user's name, typed exactly.
    confirm: String,
}

/// Deletes a user: signs them out at once, then stops and deletes their workspaces and volumes
/// and removes their devices, sessions, rules, environment, identities and settings. Irreversible.
/// The name must be typed exactly. The owner can't delete themselves. Answers 202: the account
/// shows as deleting until the workspaces are gone. Calling it again for a user being deleted is
/// harmless.
#[utoipa::path(
    delete,
    path = "/api/admin/users/{id}",
    tag = "admin",
    params(
        ("id" = i64, Path, description = "the user"),
        ("confirm" = String, Query, description = "the user's name, typed exactly")
    ),
    responses(
        (status = ACCEPTED, description = "deleting has begun"),
        (status = FORBIDDEN, description = "only the owner", body = ApiErrorBody),
        (status = NOT_FOUND, description = "no such user, or several users are not on", body = ApiErrorBody),
        (status = CONFLICT, description = "the owner, or yourself", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the name does not match", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_user(
    State(state): State<AppState>,
    principal: Principal,
    Path(id): Path<i64>,
    Query(query): Query<ConfirmDelete>,
) -> Result<StatusCode, ApiError> {
    service(&state)?
        .delete(principal.user, UserId::from_stored(id), &query.confirm)
        .await?;
    Ok(StatusCode::ACCEPTED)
}

/// Who made, re-invited or deleted which account, and when: names and dates, never a credential.
#[utoipa::path(
    get,
    path = "/api/admin/audit",
    tag = "admin",
    responses(
        (status = OK, description = "the newest lines first", body = AdminAuditList),
        (status = FORBIDDEN, description = "only the owner", body = ApiErrorBody),
        (status = NOT_FOUND, description = "several users are not on", body = ApiErrorBody)
    )
)]
pub(crate) async fn admin_audit(
    State(state): State<AppState>,
) -> Result<Json<AdminAuditList>, ApiError> {
    Ok(Json(service(&state)?.audit().await?))
}
