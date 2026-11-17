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

    /// Like [`next`](Self::next), but says when events were dropped before they were read. A source
    /// that cannot drop events keeps this default.
    fn read(&mut self) -> impl Future<Output = Read<Self::Event>> {
        async { self.next().await.map_or(Read::Closed, Read::Event) }
    }
}

/// One read of an [`EventSource`].
#[derive(Debug, PartialEq, Eq)]
pub enum Read<T> {
    /// The next event.
    Event(T),
    /// This many events were dropped because the reader fell behind.
    Lagged(u64),
    /// Every sender is gone.
    Closed,
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

/// A broadcast receiver that fell behind skips what it missed and reads on in [`next`](EventSource::next)
/// (waiting for an event to show up is still right then); [`no_event_matching`] sees the gap through
/// [`read`](EventSource::read) and fails, since a dropped event could have been the unwanted one.
impl<T: Clone + Debug> EventSource for broadcast::Receiver<T> {
    type Event = T;
    async fn next(&mut self) -> Option<T> {
        loop {
            match self.read().await {
                Read::Event(event) => return Some(event),
                Read::Lagged(_) => {}
                Read::Closed => return None,
            }
        }
    }

    async fn read(&mut self) -> Read<T> {
        match self.recv().await {
            Ok(event) => Read::Event(event),
            Err(broadcast::error::RecvError::Lagged(skipped)) => Read::Lagged(skipped),
            Err(broadcast::error::RecvError::Closed) => Read::Closed,
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
        loop {
            match source.read().await {
                Read::Event(event) => {
                    if let Some(unwanted) = pick(event) {
                        return Ok(unwanted);
                    }
                }
                Read::Lagged(skipped) => return Err(skipped),
                Read::Closed => return Err(0),
            }
        }
    })
    .await;
    match seen {
        Ok(Ok(event)) => panic!("{what}: an event came within {window:?}: {event:?}"),
        Ok(Err(skipped)) if skipped > 0 => panic!(
            "{what}: {skipped} events were dropped before they were read, so silence proves nothing; use a bigger channel"
        ),
        Ok(Err(_)) | Err(_) => {}
    }
}

#[cfg(test)]
mod tests;
