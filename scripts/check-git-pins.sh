#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# No dependency may be pinned by a bare `branch =` (docs/STANDARDS.md, "Dependencies"). A branch is a
# moving name: once the owner rebases or force-pushes it, the commit a lock file records is no
# longer on the branch's history and every fresh fetch fails with "revision not found" even though
# the lock is right. A git dependency names a tag or a full `rev` instead, and Cargo.lock records
# the same (`?tag=` or `?rev=`, never `?branch=`).
#   scripts/check-git-pins.sh              checks every tracked Cargo.toml and Cargo.lock
#   scripts/check-git-pins.sh --self-test  checks the patterns against lines they must catch or pass
set -eu
cd "$(dirname "$0")/.."

# Reads a manifest or lock file on stdin and prints each offending line, numbered. A manifest line
# offends when it sets `branch` as a key (inline table or its own line); a lock line when its git
# source carries `?branch=`.
offences() {
    awk '
        /^[[:space:]]*#/ { next }
        /^[[:space:]]*branch[[:space:]]*=/ { print NR ": " $0; next }
        /[{,][[:space:]]*branch[[:space:]]*=/ { print NR ": " $0; next }
        /^source = "git\+[^"]*\?branch=/ { print NR ": " $0 }
    '
}

if [ "${1:-}" = "--self-test" ]; then
    fail=0
    must_catch='msb_krun = { git = "https://github.com/x/libkrun", branch = "puddle" }
russh = { branch="puddle-0.63", git = "https://github.com/x/russh" }
branch = "main"
  branch = "puddle"
source = "git+https://github.com/x/libkrun?branch=puddle#daa5a6354a6e9bda6a9577dd1f13b33461763005"'
    must_pass='msb_krun = { git = "https://github.com/x/libkrun", rev = "bdf711f0c7ec5117b70c697282c61943fca36ee7" }
sdk = { git = "https://github.com/x/microsandbox", tag = "v0.7.7-puddle.19" }
source = "git+https://github.com/x/libkrun?rev=bdf711f0c7ec5117b70c697282c61943fca36ee7#bdf711f0c7ec5117b70c697282c61943fca36ee7"
source = "git+https://github.com/x/microsandbox?tag=v0.7.7-puddle.19#6ad1cdb19ea44f3f4a50af5b864a7560c8fe683b"
source = "registry+https://github.com/rust-lang/crates.io-index"
# a fork on branch = "puddle" would follow later pushes
name = "branch-and-bound"
[patch.crates-io]'
    printf '%s\n' "$must_catch" | while IFS= read -r l; do
        [ -n "$(printf '%s\n' "$l" | offences)" ] || {
            echo "self-test: should have caught: $l" >&2
            exit 1
        }
    done || fail=1
    printf '%s\n' "$must_pass" | while IFS= read -r l; do
        [ -z "$(printf '%s\n' "$l" | offences)" ] || {
            echo "self-test: should have passed: $l" >&2
            exit 1
        }
    done || fail=1
    [ "$fail" -eq 0 ] && echo "git-pins self-test ok"
    exit "$fail"
fi

bad=0
for f in $(git ls-files -- 'Cargo.toml' '*/Cargo.toml' 'Cargo.lock'); do
    [ -f "$f" ] || continue
    hits=$(offences <"$f")
    if [ -n "$hits" ]; then
        printf '%s\n' "$hits" | sed "s|^|$f:|" >&2
        bad=1
    fi
done
if [ "$bad" -ne 0 ]; then
    echo "git-pins: pin the dependency by a tag or a full rev, never a branch (scripts/check-git-pins.sh)" >&2
    exit 1
fi
echo "git pins ok"
