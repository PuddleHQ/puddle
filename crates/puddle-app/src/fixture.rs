// SPDX-License-Identifier: GPL-3.0-or-later
//! The fixture backend, for developing the window: the real API router on in-memory services, with
//! one pending request seeded so the inbox has something to show. All data is fictional. It runs no
//! workspaces, so its shutdown only stops the API.

use std::sync::Arc;

use puddle_api::{
    ApiConfig, ApiServer, ApiToken, EventHub, MemorySettings, Services, SettingsRepo,
};
use puddle_host::{HostShutdown, Step};
use puddle_store::{Limits, Store, SystemClock, UserId};
use puddle_types::{EgressRequest, Host, Problems, SuffixAllows, WorkspaceId};

use crate::backend::{Backend, BackendError};

/// The variable that sets how many terminals the fixture says are open, so the quit question can
/// be driven without a workspace.
pub(crate) const FIXTURE_TERMINALS_VAR: &str = "PUDDLE_APP_FIXTURE_TERMINALS";

/// The count `value` names; none (or text that is not a number) is zero.
fn terminals_from(value: Option<&str>) -> usize {
    value.and_then(|text| text.trim().parse().ok()).unwrap_or(0)
}

/// Starts the fixture backend on `127.0.0.1:<port>`.
pub(crate) async fn start(port: u16) -> Result<Backend, BackendError> {
    let fixture =
        |what: &str, err: &dyn std::fmt::Display| BackendError::Fixture(format!("{what}: {err}"));
    let clock = Arc::new(SystemClock);
    let store = Arc::new(
        Store::open_in_memory(clock.clone(), Limits::default())
            .map_err(|e| fixture("store", &e))?,
    );
    let workspace = WorkspaceId::new("demo").map_err(|e| fixture("workspace name", &e))?;
    let host = Host::parse_normalised("registry.example.org").map_err(|e| fixture("host", &e))?;
    // The window is the owner's, and so is the seeded workspace: one with no recorded user would
    // be nobody's, its request in no inbox.
    store
        .set_workspace_owner(&workspace, UserId::OWNER)
        .map_err(|e| fixture("owner", &e))?;
    store
        .decide(
            &EgressRequest::new(workspace, host, 443),
            SuffixAllows::Count,
        )
        .map_err(|e| fixture("seed", &e))?;
    let events = Arc::new(EventHub::default());
    let problems = Arc::new(Problems::new(events.clone()));
    let settings = Arc::new(MemorySettings::default()) as Arc<dyn SettingsRepo>;
    let services = Services::new(store.clone(), settings.clone(), events.clone(), clock)
        .with_problems(problems.clone());
    let server =
        ApiServer::bind(ApiConfig::with_port(port), ApiToken::generate()?, services).await?;
    let info = server.connection_info();
    let running = server.spawn();
    let open_terminals = terminals_from(std::env::var(FIXTURE_TERMINALS_VAR).ok().as_deref());
    Backend::new(
        info,
        events,
        problems,
        store,
        settings,
        Box::new(move || {
            Box::pin(async move {
                running.shutdown().await;
                HostShutdown {
                    steps: vec![Step::ApiStopped],
                    ..HostShutdown::default()
                }
            })
        }),
    )
    .map(|backend| backend.with_open_terminals(Arc::new(move || open_terminals)))
}

#[cfg(test)]
mod tests;
