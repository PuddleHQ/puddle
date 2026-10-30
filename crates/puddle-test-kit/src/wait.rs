// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, named polling.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;

/// The longest a single wait may be. Nextest ends a test after `terminate-after` slow periods (120 s
/// for the default profile), and a test killed that way names nothing; a wait that gives up first
/// names itself.
pub const MAX_WAIT: Duration = Duration::from_secs(100);

/// First pause between two probes; it doubles up to [`MAX_PAUSE`], so a condition that is
/// true within a few milliseconds costs a few milliseconds.
const FIRST_PAUSE: Duration = Duration::from_millis(1);
const MAX_PAUSE: Duration = Duration::from_millis(25);

/// What a probe returns: a `bool` (the wait ends on `true`, with `()`) or an `Option<T>` (it ends on
/// `Some`, with the `T`).
pub trait Outcome {
    /// What the wait returns.
    type Found;
    /// `Some` once the condition holds.
    fn found(self) -> Option<Self::Found>;
}

impl Outcome for bool {
    type Found = ();
    fn found(self) -> Option<()> {
        self.then_some(())
    }
}

impl<T> Outcome for Option<T> {
    type Found = T;
    fn found(self) -> Option<T> {
        self
    }
}

fn check_bound(what: &str, within: Duration) {
    assert!(
        within <= MAX_WAIT,
        "{what}: asked to wait {within:?}, over the {MAX_WAIT:?} a wait may take (nextest would end the test first)"
    );
}

fn give_up(what: &str, within: Duration) -> ! {
    panic!("{what}: not true within {within:?}")
}

/// Waits until `probe` says the condition holds (`true`, or `Some(value)`) and returns what it found.
/// The probe runs once at once and then after a short, growing pause. After `within` it panics with
/// `what`. Under tokio's paused clock the pauses are virtual.
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
    loop {
        match tokio::time::timeout_at(deadline, probe()).await {
            Ok(outcome) => {
                if let Some(found) = outcome.found() {
                    return found;
                }
            }
            Err(_) => give_up(what, within),
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            give_up(what, within);
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
    loop {
        if let Some(found) = probe().found() {
            return found;
        }
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            give_up(what, within);
        }
        std::thread::sleep(pause.min(left));
        pause = (pause * 2).min(MAX_PAUSE);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_true_condition_returns_at_once_without_a_pause() {
        let start = Instant::now();
        eventually("already true", Duration::from_secs(5), || true).await;
        assert_eq!(start.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn a_value_comes_back_when_the_probe_finds_it() {
        let calls = Cell::new(0);
        let got = eventually("third call", Duration::from_secs(5), || {
            calls.set(calls.get() + 1);
            (calls.get() == 3).then_some("found")
        })
        .await;
        assert_eq!(got, "found");
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "the kettle boils: not true within 2s")]
    async fn a_wait_that_runs_out_names_itself() {
        eventually("the kettle boils", Duration::from_secs(2), || false).await;
    }

    #[tokio::test(start_paused = true)]
    async fn a_wait_gives_up_at_its_bound_not_later() {
        let start = Instant::now();
        let wait = tokio::spawn(eventually("never", Duration::from_millis(300), || false));
        assert!(wait.await.is_err(), "the wait must panic");
        assert_eq!(start.elapsed(), Duration::from_millis(300));
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "slow work: not true within 1s")]
    async fn an_async_probe_that_hangs_is_cut_off_at_the_deadline() {
        eventually_async("slow work", Duration::from_secs(1), || {
            std::future::pending::<bool>()
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn an_async_probe_is_awaited() {
        let calls = Cell::new(0);
        eventually_async("second call", Duration::from_secs(1), || {
            calls.set(calls.get() + 1);
            let now = calls.get();
            async move {
                tokio::task::yield_now().await;
                now == 2
            }
        })
        .await;
    }

    #[tokio::test(start_paused = true)]
    #[should_panic(expected = "too long: asked to wait 101s")]
    async fn a_wait_over_the_cap_is_refused_before_it_starts() {
        eventually("too long", Duration::from_secs(101), || true).await;
    }

    #[test]
    fn a_blocking_wait_returns_when_the_condition_holds() {
        let mut calls = 0;
        let got = eventually_blocking("second call", Duration::from_secs(5), || {
            calls += 1;
            (calls == 2).then_some(calls)
        });
        assert_eq!(got, 2);
    }

    #[test]
    #[should_panic(expected = "never ready: not true within 30ms")]
    fn a_blocking_wait_that_runs_out_names_itself() {
        eventually_blocking("never ready", Duration::from_millis(30), || false);
    }
}
