// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading events from a channel with a bound, instead of `try_recv()` before the sender has run.

use std::fmt::Debug;
use std::future::Future;
use std::time::Duration;

use tokio::sync::{broadcast, mpsc};

use crate::wait::MAX_WAIT;

/// A channel end a test reads events from.
pub trait EventSource {
    /// What the channel carries.
    type Event: Debug;
    /// The next event, or `None` when every sender is gone.
    fn next(&mut self) -> impl Future<Output = Option<Self::Event>>;
}

impl<T: Debug> EventSource for mpsc::Receiver<T> {
    type Event = T;
    async fn next(&mut self) -> Option<T> {
        self.recv().await
    }
}

impl<T: Debug> EventSource for mpsc::UnboundedReceiver<T> {
    type Event = T;
    async fn next(&mut self) -> Option<T> {
        self.recv().await
    }
}

/// A broadcast receiver that fell behind skips what it missed and reads on: a test that needs every
/// event keeps its channel big enough.
impl<T: Clone + Debug> EventSource for broadcast::Receiver<T> {
    type Event = T;
    async fn next(&mut self) -> Option<T> {
        loop {
            match self.recv().await {
                Ok(event) => return Some(event),
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// The next event, waiting up to `within` for it. Panics with `what` when none comes, or when every
/// sender is gone first.
pub async fn next_event_within<S: EventSource>(
    what: &str,
    within: Duration,
    source: &mut S,
) -> S::Event {
    next_event_matching(what, within, source, Some).await
}

/// The first event for which `pick` returns `Some`, waiting up to `within` in all; events `pick` turns
/// down are skipped. Panics with `what` when none comes, or when every sender is gone first.
pub async fn next_event_matching<S: EventSource, T>(
    what: &str,
    within: Duration,
    source: &mut S,
    mut pick: impl FnMut(S::Event) -> Option<T>,
) -> T {
    assert!(
        within <= MAX_WAIT,
        "{what}: asked to wait {within:?}, over the {MAX_WAIT:?} a wait may take"
    );
    let found = tokio::time::timeout(within, async {
        while let Some(event) = source.next().await {
            if let Some(found) = pick(event) {
                return Some(found);
            }
        }
        None
    })
    .await;
    let Ok(found) = found else {
        panic!("{what}: no event within {within:?}");
    };
    found.unwrap_or_else(|| panic!("{what}: the channel closed before an event came"))
}

/// Shows that no event arrives for `window`: panics with `what` and the event when one does. A closed
/// channel counts as none. Every call costs the whole window, so keep it as short as the code under
/// test needs to have sent by then.
pub async fn no_event_for<S: EventSource>(what: &str, window: Duration, source: &mut S) {
    no_event_matching(what, window, source, Some).await;
}

/// [`no_event_for`] that lets events through which `pick` turns down (returns `None` for): only an
/// event it accepts fails the test, and is shown.
pub async fn no_event_matching<S: EventSource, T: Debug>(
    what: &str,
    window: Duration,
    source: &mut S,
    mut pick: impl FnMut(S::Event) -> Option<T>,
) {
    let seen = tokio::time::timeout(window, async {
        while let Some(event) = source.next().await {
            if let Some(unwanted) = pick(event) {
                return Some(unwanted);
            }
        }
        None
    })
    .await;
    if let Ok(Some(event)) = seen {
        panic!("{what}: an event came within {window:?}: {event:?}");
    }
}

#[cfg(test)]
mod tests;
