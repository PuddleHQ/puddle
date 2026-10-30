// SPDX-License-Identifier: GPL-3.0-or-later
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
    assert!(
        within <= MAX_WAIT,
        "{what}: asked to wait {within:?}, over the {MAX_WAIT:?} a wait may take"
    );
    let Ok(next) = tokio::time::timeout(within, source.next()).await else {
        panic!("{what}: no event within {within:?}");
    };
    next.unwrap_or_else(|| panic!("{what}: the channel closed before an event came"))
}

/// Shows that no event arrives for `window`: panics with `what` and the event when one does. A closed
/// channel counts as none. Every call costs the whole window, so keep it as short as the code under
/// test needs to have sent by then.
pub async fn no_event_for<S: EventSource>(what: &str, window: Duration, source: &mut S) {
    if let Ok(Some(event)) = tokio::time::timeout(window, source.next()).await {
        panic!("{what}: an event came within {window:?}: {event:?}");
    }
}

#[cfg(test)]
mod tests {
    use tokio::time::Instant;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn an_event_sent_later_is_read() {
        let (tx, mut rx) = mpsc::channel(4);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            tx.send(7).await.unwrap();
        });
        assert_eq!(
            next_event_within("seven", Duration::from_secs(1), &mut rx).await,
            7
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unbounded_and_broadcast_receivers_read_too() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("a").unwrap();
        assert_eq!(
            next_event_within("a", Duration::from_secs(1), &mut rx).await,
            "a"
        );
        let (tx, mut rx) = broadcast::channel(1);
        tx.send(1).unwrap();
        tx.send(2).unwrap();
        // The slow receiver lost 1 and reads 2.
        assert_eq!(
            next_event_within("two", Duration::from_secs(1), &mut rx).await,
            2
        );
        drop(tx);
        no_event_for("closed", Duration::from_millis(10), &mut rx).await;
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "the sign-in notice: no event within 2s")]
    async fn a_missing_event_names_the_wait() {
        let (_tx, mut rx) = mpsc::channel::<u8>(1);
        next_event_within("the sign-in notice", Duration::from_secs(2), &mut rx).await;
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "gone: the channel closed before an event came")]
    async fn a_closed_channel_is_named() {
        let (tx, mut rx) = mpsc::channel::<u8>(1);
        drop(tx);
        next_event_within("gone", Duration::from_secs(2), &mut rx).await;
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "too long: asked to wait 101s")]
    async fn a_wait_over_the_cap_is_refused() {
        let (_tx, mut rx) = mpsc::channel::<u8>(1);
        next_event_within("too long", Duration::from_secs(101), &mut rx).await;
    }

    #[tokio::test(start_paused = true)]
    async fn silence_costs_the_whole_window() {
        let (_tx, mut rx) = mpsc::channel::<u8>(1);
        let start = Instant::now();
        no_event_for("quiet", Duration::from_millis(250), &mut rx).await;
        assert_eq!(start.elapsed(), Duration::from_millis(250));
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "quiet: an event came within 1s: 9")]
    async fn an_event_inside_the_window_fails_and_is_shown() {
        let (tx, mut rx) = mpsc::channel(1);
        tx.send(9).await.unwrap();
        no_event_for("quiet", Duration::from_secs(1), &mut rx).await;
    }
}
