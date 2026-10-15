// SPDX-License-Identifier: GPL-3.0-or-later
//! A workspace's shell init: the script its login shells source (UI-17).
//!
//! The text is kept in the database and is the only source of truth. The workspace gets a
//! read-only copy at every start, and a change reaches a running workspace at once. A script that
//! prints anything when a shell starts breaks the tools that read a shell's output, so a running
//! workspace runs it once in a throwaway shell before the save is accepted.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use puddle_store::{ShellInit, ShellInitText};
use puddle_types::{WorkspaceId, WorkspaceName};

use crate::ApiErrorBody;
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::wire::{ShellInitRequest, ShellInitSaved, ShellInitView};
use crate::workspaces::ShellInitCheck;

fn view(workspace: &WorkspaceName, saved: Option<ShellInit>) -> ShellInitView {
    ShellInitView {
        workspace: workspace.clone(),
        saved: saved.is_some(),
        changed_at: saved.as_ref().map(|s| s.changed_at),
        content: saved.map_or_else(String::new, |s| s.content.as_str().to_owned()),
    }
}

/// The workspace a path names: its id and the name the store keys it by; 404 for none.
async fn named(state: &AppState, raw: &str) -> Result<(WorkspaceId, WorkspaceName), ApiError> {
    let id =
        WorkspaceId::new(raw).map_err(|_| ApiError::not_found(format!("no workspace {raw:?}")))?;
    let name = state.workspaces.get(&id).await?.name;
    Ok((id, name))
}

/// The shell init saved for a workspace; empty with `saved: false` when it has none.
#[utoipa::path(
    get,
    path = "/api/workspaces/{id}/shell-init",
    tag = "shell-init",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = OK, description = "the saved shell init", body = ShellInitView),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_shell_init(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<ShellInitView>, ApiError> {
    let (_, name) = named(&state, &id).await?;
    let shown = name.clone();
    let saved = blocking(move || Ok(state.store.shell_init(&name)?)).await?;
    Ok(Json(view(&shown, saved)))
}

/// Saves the shell init. In a running workspace the script first runs once in a throwaway shell
/// (5 seconds at most): one that does not parse, or prints anything, or runs too long is refused
/// with what was seen and nothing is saved. The new text reaches the running workspace at once;
/// a stopped one gets it when it starts, and is checked then.
#[utoipa::path(
    put,
    path = "/api/workspaces/{id}/shell-init",
    tag = "shell-init",
    params(("id" = String, Path, description = "workspace id")),
    request_body = ShellInitRequest,
    responses(
        (status = OK, description = "saved", body = ShellInitSaved),
        (status = NOT_FOUND, description = "no such workspace", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "the text is refused, or the check found a problem; the message says what", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_shell_init(
    State(state): State<AppState>,
    Path(id): Path<String>,
    crate::extract::Json(body): crate::extract::Json<ShellInitRequest>,
) -> Result<Json<ShellInitSaved>, ApiError> {
    let (workspace, name) = named(&state, &id).await?;
    let text = ShellInitText::new(&body.content)?;
    let (checked, unchecked_reason) = match state
        .workspaces
        .check_shell_init(&workspace, text.as_str())
        .await?
    {
        ShellInitCheck::Passed => (true, None),
        ShellInitCheck::NotRun(why) => (false, Some(why)),
        ShellInitCheck::Refused(why) => return Err(ApiError::invalid(why)),
    };
    let shown = name.clone();
    let saved = blocking(move || Ok(state.store.set_shell_init(&name, &text)?)).await?;
    Ok(Json(ShellInitSaved {
        shell_init: view(&shown, Some(saved)),
        checked,
        unchecked_reason,
    }))
}

/// Removes the shell init: the workspace's shells source nothing from puddle.
#[utoipa::path(
    delete,
    path = "/api/workspaces/{id}/shell-init",
    tag = "shell-init",
    params(("id" = String, Path, description = "workspace id")),
    responses(
        (status = NO_CONTENT, description = "removed"),
        (status = NOT_FOUND, description = "no such workspace, or it has no shell init", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_shell_init(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    let (_, name) = named(&state, &id).await?;
    let shown = name.to_string();
    let had = blocking(move || Ok(state.store.clear_shell_init(&name)?)).await?;
    if had {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found(format!("{shown} has no shell init")))
    }
}
