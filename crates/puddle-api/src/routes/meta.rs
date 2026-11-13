// SPDX-License-Identifier: GPL-3.0-or-later
//! Service routes.

use axum::Json;
use axum::extract::State;

use crate::routes::AppState;
use crate::wire::Health;

/// Puddle's version and the API contract's version.
#[utoipa::path(
    get,
    path = "/api/health",
    tag = "service",
    responses((status = OK, description = "Puddle is up", body = Health))
)]
pub(crate) async fn health(State(state): State<AppState>) -> Json<Health> {
    Json(Health {
        version: puddle_types::VERSION.to_owned(),
        api_version: crate::API_VERSION.to_owned(),
        ui_build_id: state.ui_build_id,
    })
}
