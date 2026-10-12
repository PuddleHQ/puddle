// SPDX-License-Identifier: GPL-3.0-or-later
//! Changing the process environment, which is only sound before a second thread exists.
//!
//! The msb SDK takes its paths from the process environment, and puddle removes the user's own
//! `MSB_*` variables so none of them redirect it. Setting variables while another thread reads
//! them is undefined behaviour on Unix, so this refuses to run once another thread exists.
//! Windows' environment calls are thread-safe, so no check is made there.

use puddle_runtime::RuntimeEnv;

use crate::HostError;

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
/// [`HostError::EnvironmentTooLate`] when the process already has more than one thread.
#[expect(
    unsafe_code,
    reason = "changing the process environment; guarded by the single-thread check"
)]
pub(crate) fn apply(plan: &RuntimeEnv) -> Result<(), HostError> {
    if !environment_may_change(thread_count()) {
        return Err(HostError::EnvironmentTooLate);
    }
    // SAFETY: on Linux the check above saw a single thread; elsewhere the platform's environment
    // calls are thread-safe (Windows) or puddle does not run (macOS is unsupported).
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
    use super::environment_may_change;

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
}
