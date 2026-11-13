// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one place Puddle swaps the process-wide panic hook.
//!
//! `std::panic::take_hook` then `set_hook` is not atomic, and the hook is shared by every thread.
//! Two callers swapping at once can drop each other's hook, or leave a silenced one in place for
//! good. Every swap takes the lock below, so they run one after another.

use std::panic::PanicHookInfo;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

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

/// Runs `make` with this thread's panic messages switched off, returning `None` if it panicked.
/// A panic on any other thread meanwhile still goes to the earlier hook, so a crash report is not
/// lost. The earlier hook is back when this returns.
///
/// `make` must not call `chain` or `silenced`: the swap lock is held for the whole call and is not
/// reentrant, so that would deadlock.
pub fn silenced<T>(make: impl FnOnce() -> T) -> Option<T> {
    let _swap = lock();
    let caller = std::thread::current().id();
    let previous = Arc::new(std::panic::take_hook());
    let forward = Arc::clone(&previous);
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() != caller {
            forward(info);
        }
    }));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(make));
    // Taking our hook out drops its clone, so `previous` has one owner again.
    drop(std::panic::take_hook());
    if let Some(hook) = Arc::into_inner(previous) {
        std::panic::set_hook(hook);
    }
    result.ok()
}

#[cfg(test)]
mod tests;
