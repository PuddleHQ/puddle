// SPDX-License-Identifier: AGPL-3.0-or-later
//! The browser arguments Puddle's WebView2 environment copies from the webview library. The library
//! keeps its defaults inside its WebView2 code, with no public constant, so Puddle repeats them in
//! `DEFAULT_ARGUMENTS`. This test rebuilds the library's defaults from the library source the lock
//! file pins, and fails when an upgrade changes them, so the copy gets updated by hand.
//! The library source is the same on every platform, so the test is not gated to one.

#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "helpers outside #[test] fns run only in tests; a panic is how a test fails"
)]

use std::fs;
use std::path::PathBuf;

const LOCK: &str = include_str!("../../../Cargo.lock");
const PUDDLE_SOURCE: &str = include_str!("../src/webview_environment.rs");

/// The text of the string literal that follows `marker` in `source`.
fn literal_after<'a>(source: &'a str, marker: &str, what: &str) -> &'a str {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("{what}: no `{marker}`"))
        + marker.len();
    let rest = &source[start..];
    let end = rest
        .find('"')
        .unwrap_or_else(|| panic!("{what}: unterminated literal after `{marker}`"));
    &rest[..end]
}

/// The version of the wry package in the lock file.
fn locked_wry_version() -> String {
    let mut lines = LOCK.lines();
    while let Some(line) = lines.next() {
        if line == "name = \"wry\"" {
            return lines
                .next()
                .and_then(|next| next.strip_prefix("version = \""))
                .and_then(|version| version.strip_suffix('"'))
                .expect("Cargo.lock: the wry package has no version line")
                .to_owned();
        }
    }
    panic!("Cargo.lock has no wry package");
}

/// One file of the locked wry crate, read from this machine's cargo registry (`cargo fetch` puts
/// every locked crate there, on every platform).
fn wry_file(version: &str, path: &str) -> String {
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".cargo"))
        })
        .expect("neither CARGO_HOME, HOME nor USERPROFILE is set");
    let registry = cargo_home.join("registry").join("src");
    let indexes = fs::read_dir(&registry)
        .unwrap_or_else(|err| panic!("{}: {err}; run `cargo fetch`", registry.display()));
    for index in indexes.flatten() {
        let file = index.path().join(format!("wry-{version}")).join(path);
        if let Ok(text) = fs::read_to_string(file) {
            return text;
        }
    }
    panic!(
        "wry {version} ({path}) is not under {}; run `cargo fetch`",
        registry.display()
    );
}

#[test]
fn default_arguments_match_the_locked_wry_defaults() {
    let version = locked_wry_version();
    let webview2 = wry_file(&version, "src/webview2/mod.rs");
    let lib = wry_file(&version, "src/lib.rs");

    // The library's rule: a base string, plus the autoplay switch when autoplay is on by default.
    let base = literal_after(&webview2, "let default_args = \"", "wry webview2");
    let autoplay_default_on = lib.lines().any(|line| line.trim() == "autoplay: true,");
    let switch = if autoplay_default_on {
        let block = webview2
            .find("if attributes.autoplay {")
            .expect("wry webview2: no `if attributes.autoplay {` block");
        literal_after(&webview2[block..], "push_str(\"", "wry webview2 autoplay")
    } else {
        ""
    };
    let wry = format!("{base}{switch}");

    let puddle = literal_after(
        PUDDLE_SOURCE,
        "DEFAULT_ARGUMENTS: &str = \"",
        "webview_environment.rs",
    );
    assert_eq!(
        puddle, wry,
        "wry {version} sets its default browser arguments to {wry:?}; \
         copy them into DEFAULT_ARGUMENTS in webview_environment.rs and check the debug-port arguments there too"
    );
}
