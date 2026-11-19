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

/// The folder of the locked wry crate, as cargo itself reports it (`cargo metadata` knows where
/// this machine keeps its sources: the registry, a vendored folder or a relocated cache).
fn wry_folder(version: &str) -> PathBuf {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--locked", "--offline"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata did not start");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("cargo metadata printed no JSON");
    let manifest = metadata
        .get("packages")
        .and_then(serde_json::Value::as_array)
        .expect("cargo metadata has no packages")
        .iter()
        .find(|p| p.get("name") == Some(&"wry".into()) && p.get("version") == Some(&version.into()))
        .and_then(|p| p.get("manifest_path")?.as_str())
        .unwrap_or_else(|| panic!("cargo metadata lists no wry {version}; run `cargo fetch`"));
    PathBuf::from(manifest)
        .parent()
        .expect("the manifest path has a folder")
        .to_path_buf()
}

/// One file of the locked wry crate.
fn wry_file(version: &str, path: &str) -> String {
    let file = wry_folder(version).join(path);
    fs::read_to_string(&file).unwrap_or_else(|err| panic!("{}: {err}", file.display()))
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
