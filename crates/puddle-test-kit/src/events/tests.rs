// SPDX-License-Identifier: AGPL-3.0-or-later
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
async fn events_that_do_not_match_are_skipped() {
    let (tx, mut rx) = mpsc::channel(4);
    for n in [1, 2, 3, 4] {
        tx.send(n).await.unwrap();
    }
    let even_after_two =
        next_event_matching("a number over two", Duration::from_secs(1), &mut rx, |n| {
            (n > 2).then_some(n * 10)
        })
        .await;
    assert_eq!(even_after_two, 30);
    no_event_matching(
        "a number over four",
        Duration::from_millis(50),
        &mut rx,
        |n| (n > 4).then_some(n),
    )
    .await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "a number over two: no event within 1s")]
async fn events_that_never_match_run_out_the_wait() {
    let (tx, mut rx) = mpsc::channel(4);
    tx.send(1).await.unwrap();
    next_event_matching("a number over two", Duration::from_secs(1), &mut rx, |n| {
        (n > 2).then_some(n)
    })
    .await;
}

#[tokio::test(start_paused = true)]
#[should_panic(expected = "a number over two: an event came within 1s: 5")]
async fn a_matching_event_inside_the_window_fails_and_is_shown() {
    let (tx, mut rx) = mpsc::channel(4);
    tx.send(1).await.unwrap();
    tx.send(5).await.unwrap();
    no_event_matching("a number over two", Duration::from_secs(1), &mut rx, |n| {
        (n > 2).then_some(n)
    })
    .await;
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
