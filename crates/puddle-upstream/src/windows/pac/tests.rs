// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use std::sync::atomic::AtomicBool;

/// Tests that count `PENDING`'s entries run one at a time: the table is process-wide.
static COUNTING: Mutex<()> = Mutex::new(());

fn live() -> usize {
    PENDING.entries().len()
}

fn call(context: usize, status: u32, info: *mut c_void) {
    // SAFETY: `context` is a lookup key; `info` is null or a live `WINHTTP_ASYNC_RESULT` for
    // the request-error status, as the callback expects.
    unsafe { callback(std::ptr::null_mut(), context, status, info, 0) };
}

#[test]
fn a_callback_fills_the_slot_of_a_resolution_that_is_waiting() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = Arc::new(Slot::default());
    let registration = Registration::new(&slot);
    call(
        registration.0,
        WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE,
        std::ptr::null_mut(),
    );
    assert_eq!(slot.wait(Duration::from_secs(1)), Some(Ok(())));

    let mut failure = WINHTTP_ASYNC_RESULT {
        dwResult: 0,
        dwError: 12002,
    };
    call(
        registration.0,
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR,
        (&raw mut failure).cast(),
    );
    assert_eq!(slot.wait(Duration::from_secs(1)), Some(Err(12002)));
}

#[test]
fn the_closing_status_releases_a_wait_for_the_handle_to_close() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = Arc::new(Slot::default());
    let registration = Registration::new(&slot);
    assert!(!slot.wait_closed(Duration::from_millis(10)));
    call(
        registration.0,
        WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
        std::ptr::null_mut(),
    );
    assert!(slot.wait_closed(Duration::from_secs(1)));
    // A closing is not an outcome.
    assert_eq!(slot.wait(Duration::from_millis(10)), None);
}

/// Sets a flag when dropped, to show whether a buffer was freed.
struct Freed(Arc<AtomicBool>);

impl Drop for Freed {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn buffers_are_held_until_the_close_is_reported_and_then_freed() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = Arc::new(Slot::default());
    let freed = Arc::new(AtomicBool::new(false));
    let closer = {
        let slot = Arc::clone(&slot);
        std::thread::spawn(move || slot.close())
    };
    let released = release_when_closed(&slot, Duration::from_secs(30), Freed(Arc::clone(&freed)));
    assert!(released, "the wait ended without the close");
    assert!(freed.load(Ordering::SeqCst));
    closer.join().expect("closer thread");
}

#[test]
fn buffers_are_leaked_when_the_close_never_comes() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = Slot::default();
    let freed = Arc::new(AtomicBool::new(false));
    let released = release_when_closed(&slot, Duration::ZERO, Freed(Arc::clone(&freed)));
    assert!(!released);
    assert!(
        !freed.load(Ordering::SeqCst),
        "freed although WinHTTP may still read it"
    );
}

#[test]
fn a_callback_after_the_resolution_ended_does_nothing() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let slot = Arc::new(Slot::default());
    let id = Registration::new(&slot).0; // the registration is gone at once: the resolution ended
    let mut failure = WINHTTP_ASYNC_RESULT {
        dwResult: 0,
        dwError: 12017,
    };
    call(
        id,
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR,
        (&raw mut failure).cast(),
    );
    call(
        id,
        WINHTTP_CALLBACK_STATUS_GETPROXYFORURL_COMPLETE,
        std::ptr::null_mut(),
    );
    assert_eq!(slot.wait(Duration::from_millis(10)), None);
}

#[test]
fn a_registration_holds_its_entry_until_it_is_dropped() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let before = live();
    let slot = Arc::new(Slot::default());
    let first = Registration::new(&slot);
    let second = Registration::new(&slot);
    assert_ne!(first.0, second.0);
    assert_eq!(live(), before + 2);
    drop(first);
    assert_eq!(live(), before + 1);
    assert!(PENDING.find(second.0).is_some());
    drop(second);
    assert_eq!(live(), before);
    // The table's own clones are gone with the entries.
    assert_eq!(Arc::strong_count(&slot), 1);
}

#[test]
fn a_timed_out_resolution_waits_for_the_close_and_ends_as_a_timeout() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let before = live();
    // A zero timeout expires before any answer can come, so this takes the timeout path.
    let query = PacQuery {
        pac_url: Some("http://127.0.0.1:1/proxy.pac".to_owned()),
        auto_detect: false,
        url: "http://example.test/".to_owned(),
        timeout: Duration::ZERO,
    };
    for _ in 0..5 {
        let result = resolve(&query);
        assert!(
            matches!(
                result,
                Err(PacError::Timeout | PacError::Failed(_) | PacError::Unavailable(_))
            ),
            "{result:?}"
        );
        assert_eq!(live(), before);
    }
}

#[test]
fn resolutions_remove_their_registration() {
    let _serial = COUNTING.lock().unwrap_or_else(PoisonError::into_inner);
    let before = live();
    // Nothing listens on port 1, so each resolution ends in an error or a timeout.
    let query = PacQuery {
        pac_url: Some("http://127.0.0.1:1/proxy.pac".to_owned()),
        auto_detect: false,
        url: "http://example.test/".to_owned(),
        timeout: Duration::from_secs(5),
    };
    for _ in 0..5 {
        let _ = resolve(&query);
        assert_eq!(live(), before);
    }
}
