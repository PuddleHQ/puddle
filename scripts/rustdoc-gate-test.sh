#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of the `rustdoc` gate on a fixture workspace of one crate, with the real cargo: a public item whose docs link to a
# private one (rustdoc::private_intra_doc_links, the warning a doc build fails on) and a link to nothing must fail the
# gate with that warning in its output; the same crate with the links fixed passes. Needs cargo and no network (the fixture has no
# dependencies).
set -eu
for v in $(git rev-parse --local-env-vars); do unset "$v"; done
unset CARGO_TARGET_DIR CHECK_TMPDIR
here=$(cd "$(dirname "$0")" && pwd)
fail() {
    echo "rustdoc-gate-test: $*" >&2
    exit 1
}
root=$(mktemp -d /var/tmp/chk-rustdoc.XXXXXX)
trap 'rm -rf "$root"' EXIT
repo=$root/repo
mkdir -p "$repo/scripts" "$repo/crates/fixture/src"
cp "$here/check.sh" "$here/gate-sched.sh" "$repo/scripts/"
# the repository's toolchain, so rustup has a version to run outside the repository (a host with no default toolchain)
cp "$here/../rust-toolchain.toml" "$repo/"
cat >"$repo/Cargo.toml" <<'T'
[workspace]
members = ["crates/fixture"]
resolver = "2"
T
cat >"$repo/crates/fixture/Cargo.toml" <<'T'
[package]
name = "fixture"
version = "0.0.0"
edition = "2021"
publish = false
T
run() { # run <gate output file>: the gate in the fixture; returns its status
    (cd "$repo" && CARGO_TARGET_DIR="$root/target" CARGO_TERM_COLOR=never scripts/check.sh rustdoc) >"$1" 2>&1
}

cat >"$repo/crates/fixture/src/lib.rs" <<'R'
/// Calls [`helper`], which is private, and [`nowhere`], which does not exist.
pub fn api() {
    helper();
}

fn helper() {}
R
(cd "$repo" && cargo generate-lockfile --offline >"$root/lock.log" 2>&1) || fail "cargo cannot write the fixture's lock file: $(tail -n 3 "$root/lock.log")"
if run "$root/bad.log"; then fail "a public doc linking to a private item and to nothing passed the gate"; fi
grep -qF 'links to private item' "$root/bad.log" || fail "the failure does not name the private link: $(tail -n 5 "$root/bad.log")"
grep -qF 'unresolved link to' "$root/bad.log" || fail "the failure does not name the broken link: $(tail -n 5 "$root/bad.log")"

cat >"$repo/crates/fixture/src/lib.rs" <<'R'
/// Calls [`helper`].
pub fn api() {
    helper();
}

/// What [`api`] calls.
pub fn helper() {}
R
run "$root/good.log" || fail "a crate with working links failed the gate: $(tail -n 5 "$root/good.log")"
echo "rustdoc-gate-test: ok"
