// SPDX-License-Identifier: GPL-3.0-or-later
//! Changing the process environment, which is only sound before a second thread exists.
//!
//! The msb SDK takes its paths from the process environment, and Puddle removes the user's own
//! `MSB_*` variables so none of them redirect it. Setting variables while another thread reads
//! them is undefined behaviour on Unix, and a C reader (`getenv`) in another thread is not
//! protected on Windows either. The host therefore calls [`mark_threads_started`] before its
//! runtime starts, and [`apply`] refuses after that on every OS. Linux also counts the threads.

use std::sync::atomic::{AtomicBool, Ordering};

use puddle_runtime::RuntimeEnv;

use crate::HostError;

/// Set once the host starts its async runtime; never cleared.
static THREADS_STARTED: AtomicBool = AtomicBool::new(false);

/// Records that this process has started, or is about to start, threads of its own, so the
/// environment can no longer be changed.
pub(crate) fn mark_threads_started() {
    THREADS_STARTED.store(true, Ordering::SeqCst);
}

/// How many threads this process runs, where the OS tells us cheaply.
#[cfg(target_os = "linux")]
fn thread_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/task")
        .ok()
        .map(Iterator::count)
}

#[cfg(not(target_os = "linux"))]
fn thread_count() -> Option<usize> {
    None
}

/// Applies `plan` to this process.
///
/// # Errors
///
/// [`HostError::EnvironmentTooLate`] after [`mark_threads_started`], or on Linux
/// when the process already has more than one thread.
#[expect(
    unsafe_code,
    reason = "changing the process environment; guarded by the threads-started flag and the thread count"
)]
pub(crate) fn apply(plan: &RuntimeEnv) -> Result<(), HostError> {
    if THREADS_STARTED.load(Ordering::SeqCst) || !environment_may_change(thread_count()) {
        return Err(HostError::EnvironmentTooLate);
    }
    // SAFETY: the flag is clear on every OS, so the host has not started its runtime, and on Linux
    // the process has a single thread. `prepare` runs before the caller starts any thread of its own.
    unsafe { plan.apply_to_process() };
    Ok(())
}

/// Whether the environment may change, given the thread count where the OS reports it. On Linux
/// an unreadable count is refused, since the single-thread check then never ran.
fn environment_may_change(threads: Option<usize>) -> bool {
    match threads {
        Some(n) => n <= 1,
        None => !cfg!(target_os = "linux"),
    }
}

#[cfg(test)]
mod tests {
    use puddle_runtime::{RuntimeEnv, RuntimeLayout};

    use super::{apply, environment_may_change, mark_threads_started};
    use crate::HostError;

    #[test]
    fn one_thread_may_change_the_environment() {
        assert!(environment_may_change(Some(1)));
    }

    #[test]
    fn several_threads_may_not() {
        assert!(!environment_may_change(Some(2)));
        assert!(!environment_may_change(Some(64)));
    }

    #[test]
    fn an_unknown_thread_count_is_refused_only_on_linux() {
        assert_eq!(environment_may_change(None), !cfg!(target_os = "linux"));
    }

    #[test]
    fn apply_refuses_once_threads_have_started() {
        let root = std::path::absolute("puddle-process-env-test").expect("a current folder");
        let layout =
            RuntimeLayout::new(root.join("runtime"), root.join("home")).expect("absolute folders");
        let plan = RuntimeEnv::plan(&layout, std::iter::empty::<(std::ffi::OsString, _)>());
        mark_threads_started();
        assert!(matches!(apply(&plan), Err(HostError::EnvironmentTooLate)));
    }
}
