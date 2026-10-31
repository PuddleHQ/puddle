// SPDX-License-Identifier: GPL-3.0-or-later
//! Test fixtures for passwords, canary strings and unique names, made at run time.
//!
//! A test that needs a password or a canary asks for one here instead of writing a literal. Each
//! test process picks one random suffix; every value carries it, so two runs never share a value
//! and no value is a constant a scanner can flag as a hard-coded credential. When any test panics,
//! the suffix is printed (the test harness shows that output for a failed test), so a failure can
//! be matched to the logs and files of that run.
//!
//! Enabled for this crate's own tests and, through the `testing` feature, for other crates'
//! dev-dependencies. Never use it for a value that comes from outside the test (a recorded
//! capture, a wire fixture): that value is fixed on purpose.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

static SUFFIX: OnceLock<String> = OnceLock::new();

/// The random suffix of this test process: 16 lowercase hex digits, the same on every call.
///
/// The first call also installs a panic hook (chained before the previous one) that prints the
/// suffix, so a failed test names the run that made its values.
pub fn suffix() -> &'static str {
    SUFFIX.get_or_init(|| {
        // `RandomState` is seeded from the OS per process; no randomness crate is needed.
        let value = RandomState::new().build_hasher().finish();
        let suffix = format!("{value:016x}");
        let shown = suffix.clone();
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            #[expect(
                clippy::print_stderr,
                reason = "the point: name the run of a failing test"
            )]
            {
                eprintln!("test fixture suffix for this run: {shown}");
            }
            previous(info);
        }));
        suffix
    })
}

/// A fixture password: `<label>-<suffix>`. The label says what it is for, and makes two passwords
/// in one test differ (`password("right")`, `password("wrong")`).
#[must_use]
pub fn password(label: &str) -> String {
    label.to_owned() + "-" + suffix()
}

/// A canary: a secret-looking string a test plants and then looks for in output, logs and files.
/// It is `CANARY-<label>-<suffix>`, so a leak is easy to grep for.
#[must_use]
pub fn canary(label: &str) -> String {
    "CANARY-".to_owned() + label + "-" + suffix()
}

/// A name no other call returns, in this process or any other: `<label>-<suffix>-<n>`, where `n`
/// counts the calls of this process. For a fixture whose name outlives it somewhere the test does
/// not control, such as a URL that a system service caches by name after the test's server is gone.
#[must_use]
pub fn unique(label: &str) -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{}-{n}", suffix())
}

/// Asserts that `text` does not contain `secret`. A failure names `what` and never prints the
/// secret or the text (which holds it when the assertion fails), so a failing leak test does not
/// itself leak into the CI log.
///
/// # Panics
///
/// When `text` contains `secret`.
#[track_caller]
pub fn assert_absent(what: &str, text: &str, secret: &str) {
    let leaked = text.contains(secret);
    assert!(
        !leaked,
        "{what} leaked a fixture secret (run suffix {})",
        suffix()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_share_one_random_suffix_and_differ_by_label() {
        let (a, b) = (password("a"), password("b"));
        assert_ne!(a, b);
        assert!(a.ends_with(suffix()) && b.ends_with(suffix()));
        assert_eq!(suffix().len(), 16);
        assert!(suffix().bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(canary("x").starts_with("CANARY-x-"));
    }

    #[test]
    fn unique_names_differ_on_every_call_and_carry_the_run_suffix() {
        let (a, b) = (unique("pac"), unique("pac"));
        assert_ne!(a, b);
        for name in [&a, &b] {
            assert!(name.starts_with(&format!("pac-{}-", suffix())), "{name}");
        }
    }

    #[test]
    fn a_clean_text_passes() {
        assert_absent("a reply", "nothing to see", &canary("clean"));
    }

    // The negative control: a canary that does appear fails the assertion, and the message
    // names the leak without printing the value.
    #[test]
    #[should_panic(expected = "the reply leaked a fixture secret")]
    fn a_planted_canary_fails_the_assertion() {
        let planted = canary("planted");
        assert_absent("the reply", &format!("body {planted} end"), &planted);
    }

    #[test]
    fn the_failure_message_omits_the_secret() {
        let planted = canary("message");
        let text = format!("body {planted}");
        let caught = std::panic::catch_unwind(|| assert_absent("the reply", &text, &planted))
            .expect_err("the assertion must fail");
        let message = caught
            .downcast_ref::<String>()
            .cloned()
            .expect("a formatted panic message");
        assert!(!message.contains(&planted));
        assert!(message.contains("the reply"));
    }
}
