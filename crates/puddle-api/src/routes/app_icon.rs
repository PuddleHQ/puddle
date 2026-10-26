// SPDX-License-Identifier: GPL-3.0-or-later
//! The app icon: the built-in icons to preview, and the user's own image. Which icon is in use is
//! the `ui.app_icon` setting; the image is kept beside the settings (`SettingsRepo`).

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use puddle_appicon::{IconError, normalize_upload};
use puddle_settings::AppIconChoice;

use crate::ApiErrorBody;
use crate::error::{ApiError, ErrorCode, blocking};
use crate::extract::File;
use crate::routes::AppState;
use crate::routes::settings::load_global;
use crate::wire::{AppIcons, BuiltInIcon, CustomIcon};

/// The side of the previews of the built-in icons.
const PREVIEW: u32 = 64;

/// The built-in icons, in the order Settings shows them.
const BUILT_IN: [AppIconChoice; 4] = [
    AppIconChoice::Duck,
    AppIconChoice::DuckPond,
    AppIconChoice::DuckNight,
    AppIconChoice::DuckSun,
];

fn png_of(icon: &puddle_appicon::Icon) -> Result<String, ApiError> {
    icon.to_png()
        .map(|png| STANDARD.encode(png))
        .map_err(|e| ApiError::internal(&e))
}

/// The built-in icons drawn to preview, and the user's own image when one was uploaded.
#[utoipa::path(
    get,
    path = "/api/app-icons",
    tag = "settings",
    responses(
        (status = OK, description = "the icons to choose between", body = AppIcons),
        (status = CONFLICT, description = "the stored image cannot be read (settings_unreadable)", body = ApiErrorBody)
    )
)]
pub(crate) async fn list_app_icons(
    State(state): State<AppState>,
) -> Result<Json<AppIcons>, ApiError> {
    let custom = blocking(move || {
        let repo = state.settings.as_ref();
        repo.load_app_icon()
            .map_err(|e| ApiError::settings_unreadable("the app icon", &e))
    })
    .await?;
    let built_in = BUILT_IN
        .into_iter()
        .map(|choice| {
            let icon = puddle_appicon::built_in(choice, PREVIEW)
                .ok_or_else(|| ApiError::internal(&"a built-in icon cannot be drawn"))?;
            Ok(BuiltInIcon {
                choice: choice.into(),
                png_base64: png_of(&icon)?,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(AppIcons {
        built_in,
        custom_png_base64: custom.map(|png| STANDARD.encode(png)),
    }))
}

/// Gives Puddle an image of the user's own to use as the app icon. The body is the image file
/// (PNG, JPEG or WebP, at most 8 MB, 32 to 8192 pixels on each side). It is checked and stored as
/// a 256 pixel square PNG, replacing an earlier one; one that is not square is fitted on a clear
/// square. This does not choose it: set `ui.app_icon` to `custom` for that.
#[utoipa::path(
    put,
    path = "/api/app-icons/custom",
    tag = "settings",
    request_body(content = Vec<u8>, content_type = "application/octet-stream", description = "the image file"),
    responses(
        (status = OK, description = "the stored icon", body = CustomIcon),
        (status = PAYLOAD_TOO_LARGE, description = "over 8 MB", body = ApiErrorBody),
        (status = UNPROCESSABLE_ENTITY, description = "not a usable image; the message says why", body = ApiErrorBody)
    )
)]
pub(crate) async fn put_custom_icon(
    State(state): State<AppState>,
    File(bytes): File,
) -> Result<Json<CustomIcon>, ApiError> {
    let lock = state.settings_lock.lock().await;
    let stored = blocking(move || {
        let png = normalize_upload(&bytes).map_err(|e| refuse(&e))?;
        state.settings.as_ref().save_app_icon(&png)?;
        tracing::info!("custom app icon stored");
        Ok(png)
    })
    .await?;
    drop(lock);
    Ok(Json(CustomIcon {
        png_base64: STANDARD.encode(stored),
    }))
}

/// Removes the user's own image. Refused while it is the icon in use: choose another icon and
/// save first, so the window never ends up with an icon that is gone.
#[utoipa::path(
    delete,
    path = "/api/app-icons/custom",
    tag = "settings",
    responses(
        (status = NO_CONTENT, description = "removed (or there was none)"),
        (status = CONFLICT, description = "it is the icon in use", body = ApiErrorBody)
    )
)]
pub(crate) async fn delete_custom_icon(
    State(state): State<AppState>,
) -> Result<StatusCode, ApiError> {
    let _lock = state.settings_lock.lock().await;
    blocking(move || {
        let repo = state.settings.as_ref();
        if load_global(repo)?.settings.ui.app_icon() == AppIconChoice::Custom {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                ErrorCode::Conflict,
                "your own image is the icon in use; choose another icon and save first",
            ));
        }
        repo.delete_app_icon()?;
        Ok(StatusCode::NO_CONTENT)
    })
    .await
}

/// A refused image as a 422 that says why.
fn refuse(err: &IconError) -> ApiError {
    match err {
        IconError::Encode(_) => ApiError::internal(err),
        _ => ApiError::invalid(err.to_string()),
    }
}
