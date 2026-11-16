// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who is told about a new request. Clients report whether they are in front and when they last
//! saw input ([`Presence::report`]); when a request opens, the server picks the client the person
//! is using, or every client that wants notifications if none was used lately, and says so once
//! on the event stream ([`Event::PendingNotify`]), so a person with several tabs, a phone and the
//! desktop app is told once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use puddle_store::{Clock, Store, UserId};
use puddle_types::{Event, EventSink, PendingSummary};
use tokio::sync::{broadcast, watch};

use crate::events::EventHub;
use crate::wire::{ClientKind, PresenceReport};

/// A client that saw input this recently is the one the person is using.
pub(crate) const ACTIVE_MS: u64 = 120_000;

/// Clients report every 30 seconds; one silent for longer than two reports and some slack is
/// gone (a closed tab, a phone that went to sleep) and is not picked.
pub(crate) const LIVE_MS: u64 = 75_000;

/// A client silent this long is forgotten.
const FORGET_MS: u64 = 600_000;

/// The most clients one user keeps; the one heard from longest ago makes room.
const MAX_CLIENTS: usize = 64;

/// The longest client id.
const MAX_ID_CHARS: usize = 64;

/// Whether `id` is a client id: 1 to 64 letters, digits, `-` or `_`.
pub(crate) fn valid_client_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_ID_CHARS
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[derive(Debug, Clone)]
struct Entry {
    kind: ClientKind,
    visible: bool,
    /// Epoch ms of the last input.
    last_input: u64,
    /// Epoch ms of the last report.
    seen: u64,
    notify: bool,
}

/// Who a new request is to be told to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Picked {
    /// The browser tabs, sorted.
    pub(crate) clients: Vec<String>,
    /// Whether the desktop app's own notification is picked.
    pub(crate) desktop: bool,
}

/// What every user's clients last reported.
#[derive(Debug, Default)]
pub(crate) struct Presence {
    users: Mutex<HashMap<UserId, HashMap<String, Entry>>>,
}

impl Presence {
    /// Records `report` from a client of `user` at `now` (epoch ms).
    pub(crate) fn report(&self, user: UserId, report: &PresenceReport, now: u64) {
        let mut users = self.users.lock().unwrap_or_else(PoisonError::into_inner);
        let clients = users.entry(user).or_default();
        clients.retain(|_, e| now.saturating_sub(e.seen) <= FORGET_MS);
        if clients.len() >= MAX_CLIENTS
            && !clients.contains_key(&report.client)
            && let Some(oldest) = clients
                .iter()
                .min_by_key(|(id, e)| (e.seen, (*id).clone()))
                .map(|(id, _)| id.clone())
        {
            clients.remove(&oldest);
        }
        clients.insert(
            report.client.clone(),
            Entry {
                kind: report.kind,
                visible: report.visible,
                last_input: now.saturating_sub(report.idle_ms),
                seen: now,
                notify: report.notify,
            },
        );
    }

    /// Who to tell about a request of `user` at `now`.
    ///
    /// Of the clients that reported lately and want notifications, the one used in the last two
    /// minutes (the most recent input; one in front wins a tie), else all of them. With no client
    /// reporting at all the desktop app is picked, as it was told before clients reported.
    pub(crate) fn pick(&self, user: UserId, now: u64) -> Picked {
        let users = self.users.lock().unwrap_or_else(PoisonError::into_inner);
        let live: Vec<(&String, &Entry)> = users
            .get(&user)
            .into_iter()
            .flatten()
            .filter(|(_, e)| now.saturating_sub(e.seen) <= LIVE_MS)
            .collect();
        if live.is_empty() {
            return Picked {
                clients: Vec::new(),
                desktop: true,
            };
        }
        let wanting: Vec<(&String, &Entry)> = live.into_iter().filter(|(_, e)| e.notify).collect();
        let in_use = wanting
            .iter()
            .filter(|(_, e)| now.saturating_sub(e.last_input) <= ACTIVE_MS)
            .max_by_key(|(id, e)| (e.last_input, e.visible, std::cmp::Reverse(*id)))
            .map(|picked| vec![*picked]);
        let chosen = in_use.unwrap_or(wanting);
        let mut clients: Vec<String> = chosen
            .iter()
            .filter(|(_, e)| e.kind == ClientKind::Browser)
            .map(|(id, _)| (*id).clone())
            .collect();
        clients.sort();
        Picked {
            clients,
            desktop: chosen.iter().any(|(_, e)| e.kind == ClientKind::Desktop),
        }
    }
}

/// Starts the picker for a server's state.
pub(crate) fn start(state: &crate::routes::AppState) -> tokio::task::JoinHandle<()> {
    spawn_picker(
        &state.events,
        state.store.clone(),
        state.presence.clone(),
        state.clock.clone(),
        state.shutdown.clone(),
    )
}

/// Follows the hub until the server stops and, after each new request, says who is to be told.
/// The task holds a tap on the hub that [`EventHub::subscribers`] does not count, and only a weak
/// reference to it, so it also ends when the hub is gone.
pub(crate) fn spawn_picker(
    events: &Arc<EventHub>,
    store: Arc<Store>,
    presence: Arc<Presence>,
    clock: Arc<dyn Clock>,
    mut shutdown: watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    let (mut rx, tap) = events.tap();
    let events = Arc::downgrade(events);
    tokio::spawn(async move {
        let _tap = tap;
        loop {
            let received = tokio::select! {
                received = rx.recv() => received,
                _ = shutdown.wait_for(|stop| *stop) => return,
            };
            match received {
                Ok(Event::PendingOpened { request }) => {
                    // A hub that is gone has nobody left to tell.
                    let Some(events) = events.upgrade() else {
                        return;
                    };
                    notify_about(&events, &store, &presence, clock.as_ref(), request).await;
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}

async fn notify_about(
    events: &EventHub,
    store: &Arc<Store>,
    presence: &Presence,
    clock: &dyn Clock,
    request: PendingSummary,
) {
    let lookup = store.clone();
    let workspace = request.workspace.clone();
    // A workspace with no recorded user is nobody's: nobody is told.
    let Ok(Some(owner)) =
        crate::error::blocking(move || Ok(lookup.workspace_owner(&workspace)?)).await
    else {
        return;
    };
    let picked = presence.pick(owner, clock.now_ms());
    events.emit(Event::PendingNotify {
        request,
        clients: picked.clients,
        desktop: picked.desktop,
    });
}

#[cfg(test)]
mod tests;
