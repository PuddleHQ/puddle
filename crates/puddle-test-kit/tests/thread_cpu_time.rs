// SPDX-License-Identifier: GPL-3.0-or-later
//! `cpu_time` counts work, not waiting: the property a CPU bar relies on, and that it still sees a
//! planted slowdown.

use std::time::Duration;

use cpu_time::ThreadTime;
use puddle_test_kit::cpu_time;

/// Spins until this thread has used `cpu` of CPU time, so a starved machine cannot shorten the work.
fn spin_for_cpu(cpu: Duration) {
    let start = ThreadTime::now();
    while start.elapsed() < cpu {
        std::hint::spin_loop();
    }
}

#[test]
fn it_returns_the_result_of_the_work() {
    let (value, _) = cpu_time(|| 6 * 7);
    assert_eq!(value, 42);
}

#[test]
fn a_planted_slowdown_is_counted() {
    // 150 ms of spinning stands in for the slowdown; 100 ms leaves room for the Windows clock's 15 ms ticks.
    let ((), cpu) = cpu_time(|| spin_for_cpu(Duration::from_millis(150)));
    assert!(cpu >= Duration::from_millis(100), "counted only {cpu:?}");
}

#[test]
fn waiting_is_not_counted() {
    // Simulated waiting: the thread is off the CPU for the whole sleep.
    let ((), cpu) = cpu_time(|| std::thread::sleep(Duration::from_millis(400)));
    // A sleep that burned CPU would show all 400 ms; 200 ms is half of that, and a quantum of the
    // Windows clock (15 ms) is far below it.
    assert!(
        cpu < Duration::from_millis(200),
        "slept, yet counted {cpu:?}"
    );
}
