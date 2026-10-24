// SPDX-License-Identifier: GPL-3.0-or-later
//! A panic writes a crash file and still reaches the hook that was there before. This is its own
//! test program because the panic hook is process-wide.

use std::sync::atomic::{AtomicBool, Ordering};

use puddle_app::crash;

static PREVIOUS_RAN: AtomicBool = AtomicBool::new(false);

#[test]
fn a_panic_in_any_thread_writes_a_crash_file_and_the_earlier_hook_still_runs() {
    let data = tempfile::tempdir().unwrap();
    let folder = data.path().join(crash::FOLDER);
    std::panic::set_hook(Box::new(|_| PREVIOUS_RAN.store(true, Ordering::SeqCst)));
    crash::install(folder.clone());

    let thread = std::thread::Builder::new()
        .name("worker".to_owned())
        .spawn(|| std::panic::panic_any("the disk fell out"))
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
    let data = tempfile::tempdir().unwrap();
    let folder = data.path().join(crash::FOLDER);
    crash::install(folder.clone());
    let _ = std::thread::spawn(|| std::panic::panic_any(42_u32)).join();
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
