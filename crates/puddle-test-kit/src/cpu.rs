// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU time of the calling thread, for bars on how much work a call does rather than how long it took.
//!
//! A wall-clock bar on a synchronous call measures the scheduler as well: on a starved machine the
//! thread is descheduled in the middle and the call "takes" ten times as long. The thread's own CPU
//! time does not count the time it spent waiting to run, so a bar on it holds under load and still
//! fails when the call does more work (a per-row query where one query was meant). The clock is
//! `CLOCK_THREAD_CPUTIME_ID` on Unix and `GetThreadTimes` on Windows (through the `cpu-time` crate),
//! which ticks in scheduler quanta (about 15 ms), so a bar below 50 ms is too fine for it.
//!
//! The call must stay on the thread that started it: a closure that hands work to another thread (a
//! runtime task, a pool) is not counted for that work.

use std::time::Duration;

use cpu_time::ThreadTime;

/// Runs `work` on the calling thread and returns its result with the CPU time this thread spent on it.
///
/// Panics when the OS refuses the clock (the Windows call needs no privilege, so a refusal means
/// the test machine is broken).
pub fn cpu_time<T>(work: impl FnOnce() -> T) -> (T, Duration) {
    let before = ThreadTime::now();
    let out = work();
    (out, before.elapsed())
}
