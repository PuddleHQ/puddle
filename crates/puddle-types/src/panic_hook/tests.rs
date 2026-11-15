// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

// The only test in this binary that touches the hook, so it cannot disturb another.
#[test]
fn a_chained_hook_runs_and_survives_a_silenced_call() {
    static RAN: AtomicBool = AtomicBool::new(false);
    static OTHER: AtomicBool = AtomicBool::new(false);
    chain(|_| RAN.store(true, Ordering::SeqCst));
    assert_eq!(silenced(|| -> u8 { panic!("quiet") }), None);
    assert!(!RAN.load(Ordering::SeqCst), "silenced hides the panic");
    assert_eq!(silenced(|| 7), Some(7));

    // A panic on another thread during `silenced` still reaches the earlier hook.
    chain(|_| OTHER.store(true, Ordering::SeqCst));
    let got = silenced(|| {
        let _ = std::thread::spawn(|| panic!("other thread")).join();
        3
    });
    assert_eq!(got, Some(3));
    assert!(
        OTHER.load(Ordering::SeqCst),
        "another thread's panic is not hidden"
    );
    let _ = std::thread::spawn(|| panic!("loud")).join();
    assert!(RAN.load(Ordering::SeqCst), "the chained hook came back");
    drop(std::panic::take_hook());
}
