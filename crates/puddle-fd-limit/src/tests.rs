// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;

#[cfg(unix)]
#[test]
fn the_hard_limit_is_tried_first_then_the_macos_ceiling() {
    assert_eq!(candidates(1024, Some(524_288)), [524_288, 10_240]);
    assert_eq!(candidates(1024, Some(4096)), [4096]);
}

#[cfg(unix)]
#[test]
fn an_unlimited_hard_limit_tries_a_million_then_the_macos_ceiling() {
    assert_eq!(candidates(1024, None), [1_048_576, 10_240]);
}

#[cfg(unix)]
#[test]
fn nothing_at_or_below_the_current_soft_limit_is_tried() {
    assert_eq!(candidates(4096, Some(4096)), Vec::<u64>::new());
    assert_eq!(candidates(20_000, Some(524_288)), [524_288]);
}

#[test]
fn allows_compares_with_the_soft_limit_and_unlimited_allows_all() {
    let capped = OpenFileLimit {
        before: Some(1024),
        soft: Some(4096),
        hard: Some(4096),
    };
    assert!(capped.allows(4096) && !capped.allows(4097));
    let none = OpenFileLimit {
        before: None,
        soft: None,
        hard: None,
    };
    assert!(none.allows(u64::MAX));
}

/// What `limit.log(needed)` wrote while it ran, one line per event with its level.
fn logged(limit: &OpenFileLimit, needed: u64) -> String {
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, || limit.log(needed));
    std::io::Write::flush(&mut capture.clone()).unwrap();
    let bytes = capture.0.lock().unwrap().clone();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn a_roomy_limit_is_logged_as_info_and_a_short_one_as_a_warning_with_the_fix() {
    let limit = OpenFileLimit {
        before: Some(1024),
        soft: Some(4096),
        hard: Some(4096),
    };
    let roomy = logged(&limit, 4096);
    assert!(roomy.contains("INFO"), "{roomy}");
    assert!(
        roomy.contains("limit=open files: soft 4096 (was 1024), hard 4096"),
        "{roomy}"
    );
    assert!(!roomy.contains("WARN"), "{roomy}");

    let short = logged(&limit, 8192);
    assert!(short.contains("WARN"), "{short}");
    assert!(short.contains("needed=8192"), "{short}");
    assert!(short.contains("ulimit -Hn"), "{short}");
    assert!(!short.contains("INFO"), "{short}");
}

#[test]
fn display_says_what_changed() {
    let limit = OpenFileLimit {
        before: Some(1024),
        soft: Some(8192),
        hard: None,
    };
    assert_eq!(
        limit.to_string(),
        "open files: soft 8192 (was 1024), hard unlimited"
    );
}
