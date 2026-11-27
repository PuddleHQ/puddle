// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[test]
fn system_clock_is_after_2026() {
    assert!(SystemClock.now_ms() > 1_767_225_600_000);
}

#[test]
fn manual_clock_moves_only_when_told() {
    let clock = ManualClock::new(5);
    assert_eq!(clock.now_ms(), 5);
    clock.advance(10);
    assert_eq!(clock.now_ms(), 15);
    clock.set(2);
    assert_eq!(clock.now_ms(), 2);
}

#[test]
fn manual_clock_monotonic_follows_forward_moves_and_ignores_a_step_back() {
    let clock = ManualClock::new(1_000);
    assert_eq!(clock.monotonic_ms(), 1_000);
    clock.advance(500);
    assert_eq!(clock.monotonic_ms(), 1_500);
    clock.set(10_000);
    assert_eq!(clock.monotonic_ms(), 10_000);
    clock.set(2_000);
    assert_eq!(clock.now_ms(), 2_000);
    assert_eq!(
        clock.monotonic_ms(),
        10_000,
        "a step back is not elapsed time"
    );
    clock.advance(7);
    assert_eq!(clock.monotonic_ms(), 10_007);
    clock.set(5_000);
    assert_eq!(
        clock.monotonic_ms(),
        13_000,
        "a step forward after one back counts only the gap"
    );
}

#[test]
fn system_clock_monotonic_counts_real_elapsed_time() {
    let first = SystemClock.monotonic_ms();
    std::thread::sleep(std::time::Duration::from_millis(30));
    let second = SystemClock.monotonic_ms();
    assert!((30..5_000).contains(&(second - first)), "{first} {second}");
}

#[test]
fn a_clock_with_no_monotonic_source_reads_its_wall_time() {
    struct Wall;
    impl Clock for Wall {
        fn now_ms(&self) -> u64 {
            42
        }
    }
    assert_eq!(Wall.monotonic_ms(), 42);
}
