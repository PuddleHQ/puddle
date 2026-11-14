// SPDX-License-Identifier: AGPL-3.0-or-later
use std::cell::Cell;
use std::sync::Mutex;

use super::*;

fn problems() -> (ProblemCallback, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let callback: ProblemCallback = Arc::new(move |why| sink.lock().unwrap().push(why));
    (callback, seen)
}

/// The wait results `WaitForMultipleObjects` gives over `[changed, stop]`.
const CHANGED: u32 = WAIT_OBJECT_0;
const STOP: u32 = WAIT_OBJECT_0 + 1;
const WAIT_FAILED: u32 = 0xFFFF_FFFF;

#[test]
fn wait_results_mean_a_change_a_stop_or_a_failure() {
    assert_eq!(Wake::of(CHANGED), Wake::Changed);
    assert_eq!(Wake::of(STOP), Wake::Stop);
    assert_eq!(Wake::of(WAIT_FAILED), Wake::Failed(WAIT_FAILED));
    assert_eq!(Wake::of(0x102), Wake::Failed(0x102));
}

#[test]
fn the_loop_reports_each_change_and_ends_quietly_when_asked_to_stop() {
    let waits = Cell::new(0);
    let changes = Cell::new(0);
    let (_, seen) = problems();
    let sink = Arc::clone(&seen);
    watch_loop(
        || ERROR_SUCCESS,
        || {
            waits.set(waits.get() + 1);
            if waits.get() < 3 { CHANGED } else { STOP }
        },
        &|| changes.set(changes.get() + 1),
        &move |why| sink.lock().unwrap().push(why),
    );
    assert_eq!((waits.get(), changes.get()), (3, 2));
    assert!(seen.lock().unwrap().is_empty(), "a stop is not a problem");
}

#[test]
fn a_notification_that_cannot_be_armed_ends_the_loop_and_says_so() {
    let arms = Cell::new(0);
    let (_, seen) = problems();
    let sink = Arc::clone(&seen);
    watch_loop(
        || {
            arms.set(arms.get() + 1);
            if arms.get() == 1 { ERROR_SUCCESS } else { 6 }
        },
        || CHANGED,
        &|| {},
        &move |why| sink.lock().unwrap().push(why),
    );
    assert_eq!(arms.get(), 2);
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(
        seen[0].contains("RegNotifyChangeKeyValue failed (error 6)"),
        "{seen:?}"
    );
}

#[test]
fn a_failed_wait_ends_the_loop_and_says_so() {
    let (_, seen) = problems();
    let sink = Arc::clone(&seen);
    watch_loop(|| ERROR_SUCCESS, || WAIT_FAILED, &|| {}, &move |why| {
        sink.lock().unwrap().push(why);
    });
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "{seen:?}");
    assert!(seen[0].contains("waiting for it failed"), "{seen:?}");
}

#[test]
fn a_key_that_cannot_be_opened_says_which_and_why() {
    let (on_problem, _) = problems();
    let missing = r"Software\puddle-test-no-such-internet-settings-key";
    let Err(why) = RegistryWatch::start_at(missing, Arc::new(|| {}), &on_problem) else {
        panic!("a key that does not exist cannot be watched");
    };
    assert!(why.contains(missing) && why.contains("error 2"), "{why}");
}

#[test]
fn no_working_part_gives_no_watch_and_every_part_that_failed_is_reported() {
    let (on_problem, seen) = problems();
    let none = assemble(
        Err("winhttp part failed".into()),
        Err("registry part failed".into()),
        &on_problem,
    );
    assert!(none.is_none());
    assert_eq!(
        *seen.lock().unwrap(),
        ["winhttp part failed", "registry part failed"]
    );
}

#[test]
fn one_working_part_is_a_watch_and_the_other_is_still_reported() {
    let (on_problem, seen) = problems();
    let registry = RegistryWatch::start(Arc::new(|| {}), &on_problem)
        .expect("the Internet Settings key can be watched on every Windows");
    let guard = assemble(Err("winhttp part failed".into()), Ok(registry), &on_problem);
    assert!(guard.is_some());
    assert_eq!(*seen.lock().unwrap(), ["winhttp part failed"]);
}

/// An unregister call that fails the way WinHTTP may (an error code).
///
/// # Safety
///
/// Any `registration` is fine: this fake never reads or dereferences it.
unsafe extern "system" fn unregister_fails(_registration: *const c_void) -> u32 {
    6 // ERROR_INVALID_HANDLE
}

/// An unregister call that succeeds.
///
/// # Safety
///
/// Any `registration` is fine: this fake never reads or dereferences it.
unsafe extern "system" fn unregister_succeeds(_registration: *const c_void) -> u32 {
    0
}

fn counting_callback() -> (ChangeCallback, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let on_change: ChangeCallback = Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    });
    (on_change, calls)
}

#[test]
fn a_callback_after_the_watch_is_dropped_finds_no_entry_and_does_nothing() {
    for unregister in [unregister_succeeds, unregister_fails] {
        let (on_change, calls) = counting_callback();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        CALLBACKS.add(id, Arc::clone(&on_change));
        let watch = WinHttpWatch {
            registration: std::ptr::null_mut(),
            unregister,
            id,
        };
        // SAFETY: the context is only an id.
        unsafe { proxy_changed(0, id as *const c_void) };
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(watch);
        assert_eq!(Arc::strong_count(&on_change), 1, "the entry was released");
        // WinHTTP calls once more after the unregister call.
        // SAFETY: as above.
        unsafe { proxy_changed(0, id as *const c_void) };
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn removing_a_callback_that_removes_another_entry_when_dropped_does_not_deadlock() {
    struct RemovesOnDrop(&'static Callbacks, usize);
    impl Drop for RemovesOnDrop {
        fn drop(&mut self) {
            self.0.remove(self.1);
        }
    }
    let callbacks: &'static Callbacks = Box::leak(Box::new(Callbacks::new()));
    let (other, _) = counting_callback();
    callbacks.add(2, other);
    let owner = RemovesOnDrop(callbacks, 2);
    callbacks.add(
        1,
        Arc::new(move || {
            let _keep = &owner;
        }),
    );
    callbacks.remove(1);
    assert!(callbacks.entries().is_empty());
}

#[test]
fn a_callback_reaches_only_its_own_registration() {
    let callbacks = Callbacks::new();
    let (first, first_calls) = counting_callback();
    let (second, second_calls) = counting_callback();
    callbacks.add(1, first);
    callbacks.add(2, second);
    callbacks.fire(2);
    assert_eq!(first_calls.load(Ordering::SeqCst), 0);
    assert_eq!(second_calls.load(Ordering::SeqCst), 1);
}

#[test]
fn registrations_leave_nothing_behind_however_often_they_run() {
    let (on_change, _calls) = counting_callback();
    for _ in 0..200 {
        let watch = WinHttpWatch::register(&on_change)
            .expect("WinHTTP proxy change notification exists on supported Windows");
        drop(watch);
    }
    // Other tests register in parallel, so only this test's own callback is counted.
    assert_eq!(
        Arc::strong_count(&on_change),
        1,
        "no entry keeps the callback"
    );
}

#[test]
fn the_real_winhttp_registration_does_not_keep_the_callback_alive() {
    let (on_change, calls) = counting_callback();
    let watch = WinHttpWatch::register(&on_change)
        .expect("WinHTTP proxy change notification exists on supported Windows");
    assert_eq!(Arc::strong_count(&on_change), 2, "the watch keeps it alive");
    drop(watch);
    assert_eq!(Arc::strong_count(&on_change), 1, "and lets go once dropped");
    drop(on_change);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
