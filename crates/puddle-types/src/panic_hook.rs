// SPDX-License-Identifier: GPL-3.0-or-later
//! The one place Puddle swaps the process-wide panic hook.
//!
//! `std::panic::take_hook` then `set_hook` is not atomic, and the hook is shared by every thread.
//! Two callers swapping at once can drop each other's hook, or leave a silenced one in place for
//! good. Every swap takes the lock below, so they run one after another.

use std::panic::PanicHookInfo;
use std::sync::{Mutex, MutexGuard, PoisonError};

static SWAP: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    // A panic never leaves the hook half swapped, so a poisoned lock is still good.
    SWAP.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Installs `extra` in front of the current hook: it runs first, then the earlier hook.
pub fn chain(extra: impl Fn(&PanicHookInfo<'_>) + Send + Sync + 'static) {
    let _swap = lock();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        extra(info);
        previous(info);
    }));
}

/// Runs `make` with panic messages switched off, returning `None` if it panicked. The earlier
/// hook is back when this returns, and no other swap happens meanwhile.
pub fn silenced<T>(make: impl FnOnce() -> T) -> Option<T> {
    let _swap = lock();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(make));
    std::panic::set_hook(previous);
    result.ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    // The only test in this binary that touches the hook, so it cannot disturb another.
    #[test]
    fn a_chained_hook_runs_and_survives_a_silenced_call() {
        static RAN: AtomicBool = AtomicBool::new(false);
        chain(|_| RAN.store(true, Ordering::SeqCst));
        assert_eq!(silenced(|| -> u8 { panic!("quiet") }), None);
        assert!(!RAN.load(Ordering::SeqCst), "silenced hides the panic");
        assert_eq!(silenced(|| 7), Some(7));
        let _ = std::thread::spawn(|| panic!("loud")).join();
        assert!(RAN.load(Ordering::SeqCst), "the chained hook came back");
        drop(std::panic::take_hook());
    }
}
