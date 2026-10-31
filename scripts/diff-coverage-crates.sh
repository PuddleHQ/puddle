#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Names the crates whose Rust files a change touches, for `scripts/check.sh diff-coverage-fast`.
# usage: diff-coverage-crates.sh <base commit>
# Prints one package name per line (sorted), measuring the working tree against the base the way the
# diff-coverage script does. Prints `*` alone when a changed .rs file belongs to no crate under crates/
# (nothing to narrow the test run to), and nothing when no .rs file changed. A change to Cargo.toml,
# Cargo.lock, rust-toolchain.toml or .cargo/ names no crate: the diff-coverage gate only looks at added
# .rs lines, and those sit in a crate that is named anyway.
# Not seen: a line one crate adds that only another crate's tests run; that crate is not in the list,
# so the fast gate reports the line as uncovered and the full `coverage` gate decides.
set -eu
[ "$#" -eq 1 ] || {
    echo "usage: diff-coverage-crates.sh <base commit>" >&2
    exit 2
}
base=$1
dirs=
outside=
for f in $(git diff --name-only --diff-filter=ACMR "$base" -- '*.rs'); do
    case $f in
    crates/*/*)
        rest=${f#crates/}
        dirs="$dirs ${rest%%/*}"
        ;;
    *) outside=1 ;;
    esac
done
if [ -n "$outside" ]; then
    echo '*'
    exit 0
fi
# shellcheck disable=SC2086 # $dirs is a list of directory names
for d in $(printf '%s\n' $dirs | sort -u); do
    manifest=crates/$d/Cargo.toml
    [ -f "$manifest" ] || continue # a crate the change deletes has nothing to run
    name=$(sed -n '/^\[package\]/,/^\[/{s/^name[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p;}' "$manifest" | head -n 1)
    [ -n "$name" ] || {
        echo "diff-coverage-crates: no package name in $manifest" >&2
        exit 1
    }
    echo "$name"
done | sort
