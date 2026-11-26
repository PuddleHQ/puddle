// SPDX-License-Identifier: AGPL-3.0-or-later
//! The system check (`puddle doctor`) as a route: the first-run flow and Settings run it.

use axum::Json;
use axum::extract::State;
use serde::Deserialize;

use crate::extract::Query;
use crate::routes::AppState;
use puddle_services::error::ApiError;
use puddle_services::error::ApiErrorBody;
use puddle_services::wire::DoctorReport;

/// Which checks to run.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DoctorQuery {
    boot: Option<bool>,
}

/// Checks this computer for what Puddle needs (virtualization, the hypervisor, the bundled
/// runtime, a test boot) and says the exact fix for each problem. It needs no administrator
/// rights. With the test boot it takes a few seconds and up to about 30; checks that overlap run
/// one after another.
#[utoipa::path(
    get,
    path = "/api/doctor",
    tag = "doctor",
    params(
        ("boot" = Option<bool>, Query, description = "include the test boot of a tiny virtual machine, the slowest check (default true)")
    ),
    responses(
        (status = OK, description = "what was found", body = DoctorReport),
        (status = BAD_REQUEST, description = "invalid query", body = ApiErrorBody),
        (status = SERVICE_UNAVAILABLE, description = "the system check is not available in this build", body = ApiErrorBody)
    )
)]
pub(crate) async fn doctor(
    State(state): State<AppState>,
    Query(query): Query<DoctorQuery>,
) -> Result<Json<DoctorReport>, ApiError> {
    Ok(Json(state.doctor.run(query.boot.unwrap_or(true)).await?))
}
