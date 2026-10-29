#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Proves the clippy half of the flake rules: clippy.toml's `disallowed-methods` flags the original code of a
# fixed flake (a test file named from the clock inside `std::env::temp_dir()`, from the windows store tests),
# the error names the fix, and `#[expect(clippy::disallowed_methods, reason = ...)]` is the one escape.
# The workspace level in Cargo.toml is "allow" while the existing sites are moved, so this runs the rule at
# "warn" in a small crate that uses the repository's clippy.toml and toolchain.
# usage: scripts/flake-lints-test.sh
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

grep -Eq '^disallowed_methods = "(allow|warn|deny)"' "$root/Cargo.toml" || {
    echo "flake-lints-test: Cargo.toml no longer sets the disallowed_methods level" >&2
    exit 1
}

mkdir -p "$tmp/src"
cp "$root/clippy.toml" "$root/rust-toolchain.toml" "$tmp/"
cat >"$tmp/Cargo.toml" <<'TOML'
[package]
name = "flake_lints_fixture"
version = "0.0.0"
edition = "2021"
publish = false
TOML
cat >"$tmp/src/lib.rs" <<'RS'
//! The original line is from the windows store tests before the fix.

pub fn root_sync_file(tag: &str, nanos: u128) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("puddle-root-sync-{tag}-{nanos}.cer"))
}

#[expect(clippy::disallowed_methods, reason = "the user's own temp folder is where the cache lives")]
pub fn cache_root() -> std::path::PathBuf {
    std::env::temp_dir()
}
RS

out=$(cd "$tmp" && CARGO_TARGET_DIR="$tmp/target" cargo clippy --offline --quiet -- -W clippy::disallowed_methods 2>&1) || {
    echo "$out" >&2
    echo "flake-lints-test: the fixture crate did not build" >&2
    exit 1
}
flagged=$(printf '%s\n' "$out" | grep -c 'use of a disallowed method' || true)
[ "$flagged" -eq 1 ] || {
    echo "$out" >&2
    echo "flake-lints-test: want 1 disallowed temp_dir (the original line; the #[expect] one is allowed), got $flagged" >&2
    exit 1
}
printf '%s\n' "$out" | grep -q 'src/lib.rs:4' || {
    echo "$out" >&2
    echo "flake-lints-test: the warning is not on the original line (src/lib.rs:4)" >&2
    exit 1
}
printf '%s\n' "$out" | grep -qF 'make the file or folder with' || {
    echo "$out" >&2
    echo "flake-lints-test: the warning does not name the fix" >&2
    exit 1
}
printf '%s\n' "$out" | grep -qF tempfile || {
    echo "flake-lints-test: the warning does not name tempfile" >&2
    exit 1
}
echo "flake-lints-test pass"
