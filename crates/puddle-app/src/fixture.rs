// SPDX-License-Identifier: GPL-3.0-or-later
//! The fixture backend, for developing the window: the real API router on in-memory services, with
//! one pending request seeded so the inbox has something to show. All data is fictional. It runs no
//! workspaces, so its shutdown only stops the API.

use std::sync::Arc;

use puddle_api::{
    ApiConfig, ApiServer, ApiToken, EventHub, MemorySettings, Services, SettingsRepo,
};
use puddle_host::{HostShutdown, Step};
use puddle_store::{Limits, Store, SystemClock};
use puddle_types::{EgressRequest, Host, Problems, SuffixAllows, WorkspaceName};

use crate::backend::{Backend, BackendError};

/// Starts the fixture backend on `127.0.0.1:<port>`.
pub(crate) async fn start(port: u16) -> Result<Backend, BackendError> {
    let fixture =
        |what: &str, err: &dyn std::fmt::Display| BackendError::Fixture(format!("{what}: {err}"));
    let clock = Arc::new(SystemClock);
    let store = Arc::new(
        Store::open_in_memory(clock.clone(), Limits::default())
            .map_err(|e| fixture("store", &e))?,
    );
    let workspace = WorkspaceName::new("demo").map_err(|e| fixture("workspace name", &e))?;
    let host = Host::parse_normalised("registry.example.org").map_err(|e| fixture("host", &e))?;
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
}
