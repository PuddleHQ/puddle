#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of scripts/diff-coverage-crates.sh in a scratch repository with three crates. Cases: no change,
# a change that adds a file to one crate (the crate alone), changes in two crates (both, sorted by package
# name, which differs from the directory name for one), a crate's tests/ file (its crate), workspace-wide
# files beside a crate change (only the crate), manifests only (nothing), a .rs file outside crates/ (`*`),
# a non-Rust file (nothing) and a deleted crate (nothing). Not seen: the cargo run that follows in check.sh.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
# shellcheck disable=SC2046 # the list is words by design
unset $(git rev-parse --local-env-vars)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/diff-coverage-crates-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "diff-coverage-crates-test: $*" >&2
    exit 1
}
git init -q "$tmp/repo"
cd "$tmp/repo"
git config user.email t@example.org
git config user.name t
mkdir -p crates/alpha/src crates/beta/src crates/gamma/src .cargo
printf '[package]\nname = "alpha"\n\n[dependencies]\nname = "not-this"\n' >crates/alpha/Cargo.toml
printf '[package]\nname = "puddle-beta"\n' >crates/beta/Cargo.toml
printf '[package]\nname = "gamma"\n' >crates/gamma/Cargo.toml
for c in alpha beta gamma; do echo 'fn a() {}' >"crates/$c/src/lib.rs"; done
echo '[workspace]' >Cargo.toml
: >Cargo.lock
git add -A
git commit -q -m base
base=$(git rev-parse HEAD)

# expect <name> <wanted output>; then back to the base
expect() {
    got=$("$here/diff-coverage-crates.sh" "$base")
    [ "$got" = "$2" ] || fail "$1: got '$got', wanted '$2'"
    git reset -q --hard "$base"
    git clean -qfd
}

expect "no change" ""
echo 'fn b() {}' >crates/alpha/src/new.rs
git add -N crates/alpha/src/new.rs
expect "one crate, a new file" "alpha"
echo 'fn c() {}' >>crates/beta/src/lib.rs
echo 'fn c() {}' >>crates/alpha/src/lib.rs
expect "two crates, sorted by package name" "alpha
puddle-beta"
mkdir crates/gamma/tests
echo 'fn t() {}' >crates/gamma/tests/it.rs
git add -N crates/gamma/tests/it.rs
expect "a crate's tests directory" "gamma"
echo 'fn c() {}' >>crates/alpha/src/lib.rs
echo '# bump' >>Cargo.lock
echo '# bump' >>Cargo.toml
mkdir -p .cargo
echo '# bump' >.cargo/config.toml
git add -N .cargo/config.toml
expect "workspace-wide files beside one crate" "alpha"
echo '# only' >>Cargo.lock
echo '# only' >>crates/alpha/Cargo.toml
expect "manifests and lock only" ""
mkdir build
echo 'fn x() {}' >build/gen.rs
echo 'fn c() {}' >>crates/alpha/src/lib.rs
git add -N build/gen.rs
expect "a .rs file outside crates/ wins over a crate" "*"
echo 'text' >crates/alpha/README.md
git add -N crates/alpha/README.md
expect "a non-Rust file in a crate" ""
git rm -q -r crates/gamma
expect "a deleted crate" ""
"$here/diff-coverage-crates.sh" >/dev/null 2>&1 && fail "no argument succeeded"
echo "diff-coverage-crates-test: ok"
