// SPDX-License-Identifier: GPL-3.0-or-later
//! Settings and consents (`puddle-settings`). Documents are read through their versioning rules:
//! an older one is migrated and written back, unknown fields are kept and logged, one from a
//! newer Puddle is refused with 409.

use axum::Json;
use axum::extract::State;
use puddle_settings::{
    GLOBAL_SCHEMA_VERSION, GlobalSettings, Loaded, SettingsError, WORKSPACE_SCHEMA_VERSION,
    WorkspaceSettings, resolve,
};
use puddle_types::WorkspaceName;
use serde_json::{Value, json};

use crate::ApiErrorBody;
use crate::error::{ApiError, blocking};
use crate::extract::Path;
use crate::routes::AppState;
use crate::settings::SettingsRepo;
use crate::wire::{
    ConsentKind, ConsentRequest, Consents, GlobalSettingsRequest, GlobalSettingsView,
    SettingsLayer, WorkspaceSettingsRequest, WorkspaceSettingsView,
};

/// The document of a kind that was never saved: current, so reading it migrates and writes nothing.
fn empty(version: u32) -> Value {
    json!({ "schema_version": version })
}

/// A document that did not parse: a newer Puddle's keeps its own refusal, anything else is damage.
fn unreadable(what: &str, err: SettingsError) -> ApiError {
    match err {
        SettingsError::NewerSchema { .. } => err.into(),
        _ => ApiError::settings_unreadable(what, &err),
    }
}

pub(crate) fn load_global(repo: &dyn SettingsRepo) -> Result<Loaded<GlobalSettings>, ApiError> {
    const WHAT: &str = "the global settings";
    let document = repo
        .load_global()
        .map_err(|e| ApiError::settings_unreadable(WHAT, &e))?
        .unwrap_or_else(|| empty(GLOBAL_SCHEMA_VERSION));
    let loaded = GlobalSettings::from_document(document).map_err(|e| unreadable(WHAT, e))?;
    if !loaded.unknown_fields.is_empty() {
        tracing::warn!(fields = ?loaded.unknown_fields, "global settings have fields this Puddle doesn't know; kept");
    }
    if let Some(from) = loaded.migrated_from {
        repo.save_global(loaded.settings.to_document())?;
        tracing::info!(from, "global settings migrated");
    }
    Ok(loaded)
}

pub(crate) fn load_workspace(
    repo: &dyn SettingsRepo,
    workspace: &WorkspaceName,
) -> Result<Loaded<WorkspaceSettings>, ApiError> {
    let what = format!("the settings of workspace {workspace}");
    let document = repo
        .load_workspace(workspace)
        .map_err(|e| ApiError::settings_unreadable(&what, &e))?
        .unwrap_or_else(|| empty(WORKSPACE_SCHEMA_VERSION));
    let loaded = WorkspaceSettings::from_document(document).map_err(|e| unreadable(&what, e))?;
    if !loaded.unknown_fields.is_empty() {
        tracing::warn!(%workspace, fields = ?loaded.unknown_fields, "workspace settings have fields this Puddle doesn't know; kept");
    }
    if let Some(from) = loaded.migrated_from {
        repo.save_workspace(workspace, loaded.settings.to_document())?;
        tracing::info!(%workspace, from, "workspace settings migrated");
    }
    Ok(loaded)
}

fn workspace_view(
    workspace: WorkspaceName,
    global: &GlobalSettings,
    loaded: &Loaded<WorkspaceSettings>,
) -> WorkspaceSettingsView {
    WorkspaceSettingsView {
        workspace,
        overrides: SettingsLayer::from(&loaded.settings.overrides),
        effective: resolve(global, Some(&loaded.settings)).into(),
        unknown_fields: loaded.unknown_fields.clone(),
    }
}

/// The global settings and the effective values of a workspace without overrides.
#[utoipa::path(
    get,
    path = "/api/settings",
    tag = "settings",
    responses(
        (status = OK, description = "global settings", body = GlobalSettingsView),
        (status = CONFLICT, description = "written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_global(
    State(state): State<AppState>,
) -> Result<Json<GlobalSettingsView>, ApiError> {
    let _lock = state.settings_lock.lock().await;
    let loaded = blocking(move || load_global(state.settings.as_ref())).await?;
    Ok(Json(GlobalSettingsView::new(&loaded)))
}

/// Replaces the global settings listed in the body; `null` falls back to Puddle's default.
/// Consents are changed through `/api/consents`.
#[utoipa::path(
    put,
    path = "/api/settings",
    tag = "settings",
    request_body = GlobalSettingsRequest,
    responses(
        (status = OK, description = "the new global settings", body = GlobalSettingsView),
        (status = CONFLICT, description = "written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a value out of range, or Microsoft's server chosen without consent", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_global(
    State(state): State<AppState>,
    crate::extract::Json(body): crate::extract::Json<GlobalSettingsRequest>,
) -> Result<Json<GlobalSettingsView>, ApiError> {
    let workspaces = state.workspaces.clone();
    let after = state.clone();
    let lock = state.settings_lock.lock().await;
    let loaded = blocking(move || {
        let repo = state.settings.as_ref();
        let mut loaded = load_global(repo)?;
        body.apply_to(&mut loaded.settings)?;
        // Consent fails closed: Microsoft's server is only chosen while the recorded consent
        // covers the terms the popup shows today, not on a grant of other terms or a decline.
        if loaded.settings.vscode_server.server() == puddle_settings::ServerChoice::Microsoft
            && !loaded.settings.microsoft_consented()
        {
            return Err(ApiError::invalid(
                "vscode_server.server: Microsoft's server needs the user's consent first",
            ));
        }
        // The icon is never chosen before there is an image to show.
        if loaded.settings.ui.app_icon() == puddle_settings::AppIconChoice::Custom
            && repo.load_app_icon()?.is_none()
        {
            return Err(ApiError::invalid(
                "ui.app_icon: give Puddle an image before choosing your own icon",
            ));
        }
        repo.save_global(loaded.settings.to_document())?;
        Ok(loaded)
    })
    .await?;
    tracing::info!("global settings changed");
    workspaces.settings_changed().await;
    drop(lock);
    crate::system_managed::refresh(&after).await;
    Ok(Json(GlobalSettingsView::new(&loaded)))
}

/// One workspace's overrides and effective values. A workspace without stored settings has no
/// overrides.
#[utoipa::path(
    get,
    path = "/api/settings/workspaces/{workspace}",
    tag = "settings",
    params(("workspace" = WorkspaceName, Path, description = "workspace name")),
    responses(
        (status = OK, description = "the workspace's settings", body = WorkspaceSettingsView),
        (status = BAD_REQUEST, description = "invalid workspace name", body = ApiErrorBody),
        (status = CONFLICT, description = "written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_workspace_settings(
    State(state): State<AppState>,
    Path(workspace): Path<WorkspaceName>,
) -> Result<Json<WorkspaceSettingsView>, ApiError> {
    let _lock = state.settings_lock.lock().await;
    let view = blocking(move || {
        let repo = state.settings.as_ref();
        let global = load_global(repo)?;
        let loaded = load_workspace(repo, &workspace)?;
        Ok(workspace_view(workspace, &global.settings, &loaded))
    })
    .await?;
    Ok(Json(view))
}

/// Replaces one workspace's overrides; `null` inherits the global value.
#[utoipa::path(
    put,
    path = "/api/settings/workspaces/{workspace}",
    tag = "settings",
    params(("workspace" = WorkspaceName, Path, description = "workspace name")),
    request_body = WorkspaceSettingsRequest,
    responses(
        (status = OK, description = "the workspace's new settings", body = WorkspaceSettingsView),
        (status = BAD_REQUEST, description = "invalid workspace name", body = ApiErrorBody),
        (status = CONFLICT, description = "written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "a value out of range", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_workspace_settings(
    State(state): State<AppState>,
    Path(workspace): Path<WorkspaceName>,
    crate::extract::Json(body): crate::extract::Json<WorkspaceSettingsRequest>,
) -> Result<Json<WorkspaceSettingsView>, ApiError> {
    let workspaces = state.workspaces.clone();
    let after = state.clone();
    let lock = state.settings_lock.lock().await;
    let view = blocking(move || {
        let repo = state.settings.as_ref();
        let global = load_global(repo)?;
        let mut loaded = load_workspace(repo, &workspace)?;
        body.overrides.apply_to(&mut loaded.settings.overrides)?;
        repo.save_workspace(&workspace, loaded.settings.to_document())?;
        tracing::info!(%workspace, "workspace settings changed");
        Ok(workspace_view(workspace, &global.settings, &loaded))
    })
    .await?;
    workspaces.settings_changed().await;
    drop(lock);
    crate::system_managed::refresh(&after).await;
    Ok(Json(view))
}

/// Every consent and its state.
#[utoipa::path(
    get,
    path = "/api/consents",
    tag = "consents",
    responses(
        (status = OK, description = "the consents", body = Consents),
        (status = CONFLICT, description = "written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody)
    )
)]
pub(crate) async fn get_consents(
    State(state): State<AppState>,
) -> Result<Json<Consents>, ApiError> {
    let _lock = state.settings_lock.lock().await;
    let loaded = blocking(move || load_global(state.settings.as_ref())).await?;
    Ok(Json(Consents::from(&loaded.settings.consents)))
}

/// Records the user's answer to a consent prompt, with the terms version shown. Puddle stamps
/// the time.
///
/// An answer that leaves Microsoft's server chosen without a consent covering today's terms (a
/// decline, which is how the user withdraws it, or a record for other terms) also takes the
/// server choice back to code-server, in the same save. The server's telemetry and update
/// options stay as the user set them.
#[utoipa::path(
    put,
    path = "/api/consents/{kind}",
    tag = "consents",
    params(("kind" = ConsentKind, Path, description = "which consent")),
    request_body = ConsentRequest,
    responses(
        (status = OK, description = "every consent, after the change", body = Consents),
        (status = BAD_REQUEST, description = "unknown consent kind", body = ApiErrorBody),
        (status = CONFLICT, description = "written by a newer Puddle (newer_settings), or damaged (settings_unreadable)", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "invalid terms version", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_consent(
    State(state): State<AppState>,
    Path(kind): Path<ConsentKind>,
    crate::extract::Json(body): crate::extract::Json<ConsentRequest>,
) -> Result<Json<Consents>, ApiError> {
    let consent = body.into_consent(state.clock.now_ms())?;
    let after = state.clone();
    let lock = state.settings_lock.lock().await;
    let consents = blocking(move || {
        let repo = state.settings.as_ref();
        let mut loaded = load_global(repo)?;
        loaded.settings.consents.set(kind.into(), consent);
        if loaded.settings.vscode_server.server() == puddle_settings::ServerChoice::Microsoft
            && !loaded.settings.microsoft_consented()
        {
            loaded.settings.vscode_server.server = Some(puddle_settings::ServerChoice::CodeServer);
        }
        repo.save_global(loaded.settings.to_document())?;
        Ok(Consents::from(&loaded.settings.consents))
    })
    .await?;
    drop(lock);
    crate::system_managed::refresh(&after).await;
    tracing::info!(kind = ?kind, "consent recorded");
    Ok(Json(consents))
}

#[cfg(test)]
mod tests {
    use crate::error::ErrorCode;
    use crate::settings::SettingsRepoError;

    use super::*;

    /// A repo whose reads fail, like a file that cannot be read.
    struct Unreadable;

    impl SettingsRepo for Unreadable {
        fn load_global(&self) -> Result<Option<Value>, SettingsRepoError> {
            Err(SettingsRepoError::new(
                "/data/global.json is not valid JSON",
            ))
        }
        fn save_global(&self, _: Value) -> Result<(), SettingsRepoError> {
            Ok(())
        }
        fn load_workspace(&self, _: &WorkspaceName) -> Result<Option<Value>, SettingsRepoError> {
            Err(SettingsRepoError::new("permission denied"))
        }
        fn save_workspace(&self, _: &WorkspaceName, _: Value) -> Result<(), SettingsRepoError> {
            Ok(())
        }
        fn load_app_icon(&self) -> Result<Option<Vec<u8>>, SettingsRepoError> {
            Ok(None)
        }
        fn save_app_icon(&self, _: &[u8]) -> Result<(), SettingsRepoError> {
            Ok(())
        }
        fn delete_app_icon(&self) -> Result<(), SettingsRepoError> {
            Ok(())
        }
    }

    #[test]
    fn the_unreadable_repo_still_accepts_saves() {
        Unreadable.save_global(json!({})).unwrap();
        let name = WorkspaceName::new("box").unwrap();
        Unreadable.save_workspace(&name, json!({})).unwrap();
        assert_eq!(Unreadable.load_app_icon().unwrap(), None);
        Unreadable.save_app_icon(b"x").unwrap();
        Unreadable.delete_app_icon().unwrap();
    }

    #[test]
    fn a_repo_that_cannot_read_says_so_with_the_cause() {
        let err = load_global(&Unreadable).unwrap_err();
        assert_eq!(err.code(), ErrorCode::SettingsUnreadable);
        let message = err.message();
        assert!(message.contains("the global settings"));
        assert!(message.contains("/data/global.json is not valid JSON"));
        // The bare cause is what other reports (the workspace record) wrap in their own words.
        let reason = err.reason();
        assert!(!reason.contains("Puddle cannot read"));

        let name = WorkspaceName::new("box").unwrap();
        let err = load_workspace(&Unreadable, &name).unwrap_err();
        assert_eq!(err.code(), ErrorCode::SettingsUnreadable);
        let message = err.message();
        assert!(message.contains("workspace box"));
        assert!(message.contains("permission denied"));
    }
}
