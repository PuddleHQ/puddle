// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

const LIMIT: Duration = Duration::from_secs(100);
const GRACE: Duration = Duration::from_secs(20);
const KEEPALIVE: Duration = Duration::from_secs(60);

fn sweeper() -> Sweeper {
    Sweeper::new(
        Clock {
            keepalive: KEEPALIVE,
            grace: GRACE,
        },
        Instant::now(),
    )
}

async fn advance(secs: u64) {
    tokio::time::advance(Duration::from_secs(secs)).await;
}

#[tokio::test(start_paused = true)]
async fn a_stream_quiet_for_its_limit_is_told_to_give_up_and_a_frame_defers_it() {
    let registry = Arc::new(Registry::default());
    let guest = registry.open(LIMIT, None);
    let activity = guest.activity();
    let mut cx = TaskContext::from_waker(Waker::noop());
    assert!(activity.poll_expired(&mut cx).is_pending());
    advance(99).await;
    activity.touch();
    advance(2).await;
    registry.sweep(Instant::now(), GRACE);
    assert!(
        activity.poll_expired(&mut cx).is_pending(),
        "a frame moved a moment ago"
    );
    advance(100).await;
    registry.sweep(Instant::now(), GRACE);
    assert!(activity.poll_expired(&mut cx).is_ready());
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_moves_again_after_its_order_still_counts_as_stuck_after_the_grace() {
    let registry = Arc::new(Registry::default());
    let guard = registry.open(LIMIT, None);
    advance(101).await;
    assert!(!registry.sweep(Instant::now(), GRACE), "just told");
    advance(10).await;
    guard.activity().touch();
    assert!(!registry.sweep(Instant::now(), GRACE), "inside the grace");
    advance(11).await;
    guard.activity().touch();
    assert!(
        registry.sweep(Instant::now(), GRACE),
        "a stream that was told to give up and is still open after the grace is stuck"
    );
}

#[tokio::test(start_paused = true)]
async fn the_waker_of_a_polled_body_is_woken_when_its_stream_expires() {
    let registry = Arc::new(Registry::default());
    let guard = registry.open(LIMIT, None);
    let activity = guard.activity();
    let waiter = tokio::spawn(async move { activity.expired().await });
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    advance(101).await;
    registry.sweep(Instant::now(), GRACE);
    waiter.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn the_limit_of_a_stream_can_be_changed_for_a_pipe() {
    let registry = Arc::new(Registry::default());
    let guard = registry.open(LIMIT, None);
    guard.activity().set_limit(Duration::from_secs(10));
    advance(11).await;
    registry.sweep(Instant::now(), GRACE);
    let mut cx = TaskContext::from_waker(Waker::noop());
    assert!(guard.activity().poll_expired(&mut cx).is_ready());
}

#[tokio::test(start_paused = true)]
async fn a_dropped_guard_leaves_the_registry_and_returns_its_permit() {
    let budget = Arc::new(tokio::sync::Semaphore::new(1));
    let registry = Arc::new(Registry::default());
    let permit = Arc::clone(&budget).try_acquire_owned().unwrap();
    let guard = registry.open(LIMIT, Some(permit));
    assert_eq!(registry.len(), 1);
    assert_eq!(budget.available_permits(), 0);
    drop(guard);
    assert_eq!(registry.len(), 0);
    assert_eq!(budget.available_permits(), 1);
}

#[tokio::test(start_paused = true)]
async fn an_idle_connection_is_shut_down_after_the_keepalive_and_dropped_after_the_grace() {
    let registry = Arc::new(Registry::default());
    let mut sweep = sweeper();
    advance(59).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Nothing);
    advance(2).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Shutdown);
    advance(19).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Nothing);
    advance(2).await;
    assert_eq!(
        sweep.tick(Instant::now(), &registry),
        Step::Drop(ended::SHUTDOWN_IGNORED)
    );
}

#[tokio::test(start_paused = true)]
async fn an_open_stream_keeps_a_connection_from_being_shut_down() {
    let registry = Arc::new(Registry::default());
    let _stream = registry.open(Duration::from_secs(10_000), None);
    let mut sweep = sweeper();
    for _ in 0..10 {
        advance(30).await;
        assert_eq!(sweep.tick(Instant::now(), &registry), Step::Nothing);
    }
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_is_never_polled_ends_its_connection_in_the_grace_after_its_limit() {
    let registry = Arc::new(Registry::default());
    let _pinned = registry.open(LIMIT, None);
    let mut sweep = sweeper();
    advance(101).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Nothing);
    advance(21).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Shutdown);
    advance(10).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Nothing);
    advance(11).await;
    assert_eq!(
        sweep.tick(Instant::now(), &registry),
        Step::Drop(ended::STREAM_IDLE)
    );
}

#[tokio::test(start_paused = true)]
async fn a_stream_that_gives_way_in_time_leaves_its_connection_alone() {
    let registry = Arc::new(Registry::default());
    let pinned = registry.open(LIMIT, None);
    let mut sweep = sweeper();
    advance(101).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Nothing);
    advance(10).await;
    drop(pinned);
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Nothing);
}

#[tokio::test(start_paused = true)]
async fn streams_finishing_after_a_shutdown_get_their_time() {
    let registry = Arc::new(Registry::default());
    let mut sweep = sweeper();
    advance(61).await;
    assert_eq!(sweep.tick(Instant::now(), &registry), Step::Shutdown);
    let busy = registry.open(Duration::from_secs(10_000), None);
    advance(100).await;
    assert_eq!(
        sweep.tick(Instant::now(), &registry),
        Step::Nothing,
        "a stream that was in flight when the shutdown came may finish"
    );
    drop(busy);
    assert_eq!(
        sweep.tick(Instant::now(), &registry),
        Step::Drop(ended::SHUTDOWN_IGNORED),
        "and the connection that is still open after that has had its grace"
    );
}
