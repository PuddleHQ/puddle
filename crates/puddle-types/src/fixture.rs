// SPDX-License-Identifier: AGPL-3.0-or-later
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
/// The first call also installs a panic hook (chained before the previous one, through `panic_hook`) that prints the
/// suffix, so a failed test names the run that made its values.
pub fn suffix() -> &'static str {
    SUFFIX.get_or_init(|| {
        // `RandomState` is seeded from the OS per process; no randomness crate is needed.
        let value = RandomState::new().build_hasher().finish();
        let suffix = format!("{value:016x}");
        let shown = suffix.clone();
        crate::panic_hook::chain(move |_| {
            #[expect(
                clippy::print_stderr,
                reason = "the point: name the run of a failing test"
            )]
            {
                eprintln!("test fixture suffix for this run: {shown}");
            }
        });
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

/// A name no other call of this process returns, `<label>-<suffix>-<n>` where `n` counts the
/// calls; the random run suffix keeps other processes apart. For a fixture whose name outlives it somewhere the test does
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
mod tests;
