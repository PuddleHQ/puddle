// SPDX-License-Identifier: GPL-3.0-or-later
//! A panic writes a crash file and still reaches the hook that was there before. This is its own
//! test program because the panic hook is process-wide, and the tests in it run on parallel
//! threads: each one holds [`Hook`] for its whole body, so only one hook is installed at a time and
//! a panic never lands in another test's folder.

use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};

use puddle_app::crash;

static PREVIOUS_RAN: AtomicBool = AtomicBool::new(false);

static HOOK_LOCK: Mutex<()> = Mutex::new(());

/// Exclusive use of the process-wide panic hook; the default hook is back when it drops.
struct Hook {
    _lock: MutexGuard<'static, ()>,
}

impl Hook {
    fn take() -> Self {
        // Poison from a failed test is irrelevant: the guard resets the hook.
        Hook {
            _lock: HOOK_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        }
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        // take_hook panics on a panicking thread; a failed assert must report, not abort.
        if !std::thread::panicking() {
            drop(panic::take_hook());
        }
    }
}

#[test]
fn a_panic_in_any_thread_writes_a_crash_file_and_the_earlier_hook_still_runs() {
    let _hook = Hook::take();
    let data = tempfile::tempdir().unwrap();
    let folder = data.path().join(crash::FOLDER);
    panic::set_hook(Box::new(|_| PREVIOUS_RAN.store(true, Ordering::SeqCst)));
    crash::install(folder.clone());

    let thread = std::thread::Builder::new()
        .name("worker".to_owned())
        .spawn(|| panic::panic_any("the disk fell out"))
        .unwrap();
    assert!(thread.join().is_err());

    let files: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let text = std::fs::read_to_string(&files[0]).unwrap();
    assert!(text.contains("message:  the disk fell out"), "{text}");
    assert!(text.contains("thread:   worker"), "{text}");
    assert!(text.contains("crash_hook.rs"), "{text}");
    assert!(
        PREVIOUS_RAN.load(Ordering::SeqCst),
        "the earlier hook ran too"
    );
}

#[test]
fn a_panic_with_no_text_is_still_a_report() {
    let _hook = Hook::take();
    let data = tempfile::tempdir().unwrap();
    let folder = data.path().join(crash::FOLDER);
    crash::install(folder.clone());
    let _ = std::thread::spawn(|| panic::panic_any(42_u32)).join();
    let text = std::fs::read_to_string(
        std::fs::read_dir(&folder)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path(),
    )
    .unwrap();
    assert!(text.contains("message:  (no message)"), "{text}");
}

#[test]
fn a_crash_file_that_cannot_be_written_does_not_hide_the_panic() {
    let _hook = Hook::take();
    let data = tempfile::tempdir().unwrap();
    // A plain file where the folder should be: nothing can be created under it.
    let folder = data.path().join(crash::FOLDER);
    std::fs::write(&folder, "in the way").unwrap();
    crash::install(folder.clone());
    let thread = std::thread::spawn(|| panic::panic_any("still reported"));
    assert!(thread.join().is_err(), "the panic still unwinds the thread");
    assert_eq!(std::fs::read_to_string(&folder).unwrap(), "in the way");
}
