// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SSE event stream.

use std::time::Duration;

use axum::extract::State;
use axum::response::sse::{KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use puddle_types::WorkspaceId;
use serde::Deserialize;

use crate::authz::{Principal, Via};
use crate::events::Audience;
use crate::extract::Query;
use crate::routes::AppState;

/// How often an idle stream gets a comment line, so proxies and clients see it is alive.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Which events a subscriber wants.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EventsQuery {
    /// Only this workspace's events (global events always come through). All events if left out.
    workspace: Option<WorkspaceId>,
}

/// Server-sent events: each `data:` line is one `Event` as JSON (the `Event` schema; switch on
/// `type`). A subscriber that fell behind gets `event: lagged` with a `Lagged` body instead of
/// the events it missed. Comment lines keep an idle stream alive. Use a `fetch`-based reader:
/// `EventSource` can't send the bearer token.
#[utoipa::path(
    get,
    path = "/api/events",
    tag = "events",
    params(("workspace" = Option<WorkspaceId>, Query, description = "only this workspace's events; global events always come through")),
    responses(
        (status = OK, description = "the event stream", content_type = "text/event-stream", body = String),
        (status = BAD_REQUEST, description = "invalid workspace name", body = crate::ApiErrorBody)
    )
)]
pub(crate) async fn events(
    State(state): State<AppState>,
    principal: Principal,
    Query(query): Query<EventsQuery>,
) -> Response {
    // A signed-in browser's stream ends when its device is signed out.
    let ended: std::pin::Pin<Box<dyn Future<Output = ()> + Send>> =
        match (principal.via, &state.sign_in) {
            (Via::Device(device), Some(sign_in)) => Box::pin(sign_in.clone().ended(device)),
            _ => Box::pin(std::future::pending()),
        };
    let stream = crate::events::stream(
        state.events.subscribe(),
        Audience::new(principal, state.store.clone()),
        query.workspace,
        state.shutdown.clone(),
        ended,
    );
    Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
        .into_response()
}
