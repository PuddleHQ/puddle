// SPDX-License-Identifier: AGPL-3.0-or-later
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
async fn an_ok_ends_the_wait_with_its_value() {
    let calls = Cell::new(0);
    let got = eventually("the second look", Duration::from_secs(5), || {
        calls.set(calls.get() + 1);
        if calls.get() == 2 {
            Ok(calls.get())
        } else {
            Err("not yet")
        }
    })
    .await;
    assert_eq!(got, 2);
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "the kettle boils: not true within 2s")]
async fn a_wait_that_runs_out_names_itself() {
    eventually("the kettle boils", Duration::from_secs(2), || false).await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "the table: not true within 1s; last seen: 2 rows")]
async fn a_wait_that_runs_out_shows_the_last_thing_the_probe_saw() {
    let calls = Cell::new(0);
    eventually("the table", Duration::from_secs(1), || {
        calls.set(calls.get() + 1);
        Err::<(), _>(format!("{} rows", calls.get().min(2)))
    })
    .await;
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

#[test]
#[should_panic(expected = "never ready: not true within 30ms; last seen: 3 files")]
fn a_blocking_wait_shows_what_the_probe_saw() {
    eventually_blocking("never ready", Duration::from_millis(30), || {
        Err::<(), _>("3 files")
    });
}

#[test]
fn a_bounded_call_hands_back_its_value() {
    assert_eq!(returns_within("adds", Duration::from_secs(5), || 2 + 2), 4);
}

#[test]
#[should_panic(expected = "stuck: did not return within 30ms")]
fn a_bounded_call_that_does_not_return_names_itself() {
    // Parks for far longer than the bound; the thread is left behind and ends with the process.
    returns_within("stuck", Duration::from_millis(30), || {
        std::thread::park();
    });
}

#[test]
#[should_panic(expected = "boom: panicked before it returned")]
fn a_bounded_call_that_panics_says_so() {
    returns_within("boom", Duration::from_secs(5), || -> u8 { panic!("inner") });
}
