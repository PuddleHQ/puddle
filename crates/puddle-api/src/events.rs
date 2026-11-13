// SPDX-License-Identifier: GPL-3.0-or-later
//! The event hub: Puddle's components emit [`Event`]s into it ([`EventSink`]), and every SSE
//! subscriber gets a copy.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use axum::response::sse;
use futures_util::{Stream, StreamExt};
use puddle_store::{Role, Store, UserId};
use puddle_types::{Event, EventSink, WorkspaceId};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, watch};
use utoipa::ToSchema;

use crate::authz::Principal;

/// Events buffered per subscriber before the slowest one starts missing some.
pub const DEFAULT_EVENT_BUFFER: usize = 1024;

/// Fans events out to SSE subscribers. Emitting never blocks: with no subscriber the event is
/// dropped, and a subscriber that falls more than the buffer behind gets a [`Lagged`] notice
/// instead of the events it missed.
#[derive(Debug)]
pub struct EventHub {
    tx: broadcast::Sender<Event>,
}

impl EventHub {
    /// A hub buffering `capacity` events per subscriber (at least 1).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity.max(1));
        Self { tx }
    }

    /// How many SSE streams are open.
    #[must_use]
    pub fn subscribers(&self) -> usize {
        self.tx.receiver_count()
    }

    /// A receiver of every event emitted from now on. One that falls behind by more than the
    /// buffer is told how many it missed (`RecvError::Lagged`) and should look at the state again.
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }
}

impl Default for EventHub {
    fn default() -> Self {
        Self::new(DEFAULT_EVENT_BUFFER)
    }
}

impl EventSink for EventHub {
    fn emit(&self, event: Event) {
        // `send` fails only when nobody subscribes; then there is nobody to tell.
        let _ = self.tx.send(event);
    }
}

/// Sent on the event stream as `event: lagged` when a subscriber fell behind and events were
/// dropped for it. The client should refetch the state it shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Lagged {
    /// How many events were dropped.
    pub missed: u64,
}

/// Whether a subscriber filtering on `filter` gets `event`. Global events (no workspace) go to
/// every subscriber.
pub(crate) fn wanted(event: &Event, filter: Option<&WorkspaceId>) -> bool {
    wanted_for(event.workspace(), filter)
}

fn wanted_for(workspace: Option<&WorkspaceId>, filter: Option<&WorkspaceId>) -> bool {
    match (filter, workspace) {
        (None, _) | (_, None) => true,
        (Some(want), Some(got)) => want == got,
    }
}

/// Who one stream is for: the events of the principal's own workspaces, their own audit records,
/// the signals that carry nothing (rules, identities, the global environment or problems
/// changed), and the install's other events (the network, sign-ins) for the owner only.
pub(crate) struct Audience {
    user: UserId,
    role: Role,
    store: Arc<Store>,
    /// Each workspace's user as first looked up; it never changes.
    known: HashMap<WorkspaceId, UserId>,
}

impl Audience {
    pub(crate) fn new(principal: Principal, store: Arc<Store>) -> Self {
        Self {
            user: principal.user,
            role: principal.role,
            store,
            known: HashMap::new(),
        }
    }

    /// Whether the stream may carry `event`. A user that can't be looked up gets nothing.
    async fn sees(&mut self, event: &Event) -> bool {
        if let Event::AuditAppended { user, .. } = event {
            return *user == self.user.get();
        }
        let Some(workspace) = event.workspace() else {
            // Signals that carry nothing go to everyone, who refetch what they may see.
            return self.role == Role::Owner
                || matches!(
                    event,
                    Event::RulesChanged {}
                        | Event::RequestRulesChanged {}
                        | Event::ProblemsChanged {}
                        | Event::IdentitiesChanged {}
                        | Event::GlobalEnvChanged {}
                        | Event::RepoListsChanged {}
                );
        };
        let user = if let Some(user) = self.known.get(workspace) {
            *user
        } else {
            let store = self.store.clone();
            let id = workspace.clone();
            // A workspace with no recorded user is nobody's: its events reach no one (and are not
            // remembered, as a user is recorded before anything about a workspace is announced).
            match crate::error::blocking(move || Ok(store.workspace_owner(&id)?)).await {
                Ok(Some(user)) => *self.known.entry(workspace.clone()).or_insert(user),
                Ok(None) | Err(_) => return false,
            }
        };
        user == self.user
    }
}

fn to_sse(event: &Event) -> sse::Event {
    sse::Event::default()
        .json_data(event)
        .unwrap_or_else(|err| {
            // An `Event` always serialises; if that ever breaks, say so on the stream rather than
            // ending it.
            tracing::error!(error = %err, "event did not serialise");
            sse::Event::default()
                .event("error")
                .data("event did not serialise")
        })
}

fn lagged(missed: u64) -> sse::Event {
    sse::Event::default()
        .event("lagged")
        .json_data(Lagged { missed })
        .unwrap_or_else(|_| sse::Event::default().event("lagged").data("{}"))
}

/// The SSE stream for one subscriber. It ends when the hub is dropped, the server shuts down or
/// `ended` completes (the subscriber's device was revoked).
pub(crate) fn stream(
    rx: broadcast::Receiver<Event>,
    audience: Audience,
    filter: Option<WorkspaceId>,
    mut shutdown: watch::Receiver<bool>,
    ended: impl Future<Output = ()> + Send + 'static,
) -> impl Stream<Item = Result<sse::Event, Infallible>> {
    let events = futures_util::stream::unfold((rx, audience), move |(mut rx, mut audience)| {
        let filter = filter.clone();
        async move {
            loop {
                match rx.recv().await {
                    Ok(event) if wanted(&event, filter.as_ref()) && audience.sees(&event).await => {
                        return Some((Ok(to_sse(&event)), (rx, audience)));
                    }
                    Ok(_) => {}
                    Err(broadcast::error::RecvError::Lagged(missed)) => {
                        return Some((Ok(lagged(missed)), (rx, audience)));
                    }
                    Err(broadcast::error::RecvError::Closed) => return None,
                }
            }
        }
    });
    events.take_until(async move {
        tokio::select! {
            // A dropped sender also means "shut down".
            _ = shutdown.wait_for(|stop| *stop) => {}
            // Or the browser the stream is for was signed out.
            () = ended => {}
        }
    })
}

#[cfg(test)]
mod tests;
