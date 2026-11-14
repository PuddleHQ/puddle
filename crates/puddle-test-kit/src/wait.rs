// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded, named polling.

use std::fmt::Display;
use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

/// The longest a single wait may be. Nextest ends a test after `terminate-after` slow periods (120 s
/// for the default profile), and a test killed that way names nothing; a wait that gives up first
/// names itself.
pub const MAX_WAIT: Duration = Duration::from_secs(100);

/// First pause between two probes; it doubles up to [`MAX_PAUSE`] (the shortest pause any of the hand-rolled
/// helpers it replaced used, so no wait takes longer to notice), so a condition that is
/// true within a few milliseconds costs a few milliseconds.
const FIRST_PAUSE: Duration = Duration::from_millis(1);
const MAX_PAUSE: Duration = Duration::from_millis(10);

/// What one probe found: the wait is over, or it goes on (with what the probe saw, when it says).
#[derive(Debug)]
pub enum Step<T> {
    /// The condition holds.
    Done(T),
    /// Not yet; the text, if any, is shown when the wait runs out.
    Wait(Option<String>),
}

/// What a probe returns: a `bool` (the wait ends on `true`, with `()`), an `Option<T>` (it ends on
/// `Some`, with the `T`) or a `Result<T, E>` (it ends on `Ok`, with the `T`; the last `Err` is shown
/// when the wait runs out, so a CI log says what the code under test did instead of only that it
/// did not do the right thing).
pub trait Outcome {
    /// What the wait returns.
    type Found;
    /// Whether the condition holds.
    fn step(self) -> Step<Self::Found>;
}

impl Outcome for bool {
    type Found = ();
    fn step(self) -> Step<()> {
        if self {
            Step::Done(())
        } else {
            Step::Wait(None)
        }
    }
}

impl<T> Outcome for Option<T> {
    type Found = T;
    fn step(self) -> Step<T> {
        self.map_or(Step::Wait(None), Step::Done)
    }
}

impl<T, E: Display> Outcome for Result<T, E> {
    type Found = T;
    fn step(self) -> Step<T> {
        match self {
            Ok(found) => Step::Done(found),
            Err(seen) => Step::Wait(Some(seen.to_string())),
        }
    }
}

fn check_bound(what: &str, within: Duration) {
    assert!(
        within <= MAX_WAIT,
        "{what}: asked to wait {within:?}, over the {MAX_WAIT:?} a wait may take (nextest would end the test first)"
    );
}

fn give_up(what: &str, within: Duration, seen: Option<&str>) -> ! {
    let saw = seen.map_or_else(String::new, |s| format!("; last seen: {s}"));
    panic!("{what}: not true within {within:?}{saw}")
}

/// Waits until `probe` says the condition holds (`true`, `Some(value)` or `Ok(value)`) and returns what
/// it found. The probe runs once at once and then after a short, growing pause. After `within` it
/// panics with `what`. Under tokio's paused clock the pauses are virtual.
pub async fn eventually<O: Outcome>(
    what: &str,
    within: Duration,
    mut probe: impl FnMut() -> O,
) -> O::Found {
    eventually_async(what, within, move || std::future::ready(probe())).await
}

/// [`eventually`] for a probe that awaits. A probe that itself hangs is cut off at the deadline too.
pub async fn eventually_async<O: Outcome, F: Future<Output = O>>(
    what: &str,
    within: Duration,
    mut probe: impl FnMut() -> F,
) -> O::Found {
    check_bound(what, within);
    let deadline = Instant::now() + within;
    let mut pause = FIRST_PAUSE;
    let mut seen = None;
    loop {
        match tokio::time::timeout_at(deadline, probe()).await {
            Ok(outcome) => match outcome.step() {
                Step::Done(found) => return found,
                Step::Wait(detail) => seen = detail.or(seen),
            },
            Err(_elapsed) => give_up(what, within, seen.as_deref()),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            give_up(what, within, seen.as_deref());
        }
        tokio::time::sleep(pause.min(left)).await;
        pause = (pause * 2).min(MAX_PAUSE);
    }
}

/// [`eventually`] for a test that is not async (it sleeps the thread between probes).
pub fn eventually_blocking<O: Outcome>(
    what: &str,
    within: Duration,
    mut probe: impl FnMut() -> O,
) -> O::Found {
    check_bound(what, within);
    let deadline = std::time::Instant::now() + within;
    let mut pause = FIRST_PAUSE;
    let mut seen = None;
    loop {
        match probe().step() {
            Step::Done(found) => return found,
            Step::Wait(detail) => seen = detail.or(seen),
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            give_up(what, within, seen.as_deref());
        }
        std::thread::sleep(pause.min(left));
        pause = (pause * 2).min(MAX_PAUSE);
    }
}

/// Runs `f` on its own thread and returns what it returns; panics, naming `what`, when it has not
/// returned after `bound`. For code that must always end (a shortening loop, a scanner that must
/// advance): a regression there would otherwise spin and allocate until the process dies, which
/// reads as a hang, not a failed test. `bound` is a hang guard, not a measurement, so it stays fixed.
/// A thread that never returns is left running until the test process exits; a panic in `f` is
/// reported as such, not as a timeout.
pub fn returns_within<T: Send + 'static>(
    what: &str,
    bound: Duration,
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    use std::sync::mpsc::{RecvTimeoutError, channel};
    check_bound(what, bound);
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(bound) {
        Ok(value) => value,
        Err(RecvTimeoutError::Timeout) => {
            panic!("{what}: did not return within {bound:?}")
        }
        Err(RecvTimeoutError::Disconnected) => panic!("{what}: panicked before it returned"),
    }
}

#[cfg(test)]
mod tests;
