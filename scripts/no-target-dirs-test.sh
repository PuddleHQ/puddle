#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Checks that no tracked path has a directory named `target`. Cargo's build output is a directory
# named `target`, and many tools remove, skip or leave out a directory by that name wherever it sits:
# `find -name target`, a backup or copy that excludes `target`, a `.gitignore` line `target/`.
# A source module `target` (`src/target/tests.rs`) then vanished from a dozen task worktrees (two
# tracked files shown as deleted), which would trip land.sh's deletion check. Name the module
# something else (`request_target`).
# A file named `target.rs` is fine; only a directory component counts.
# usage: scripts/no-target-dirs-test.sh [root]   (default: the repository; `--self-test` checks the check)
set -eu

here=$(cd "$(dirname "$0")" && pwd)

# Prints every tracked path with a directory component named `target`.
offenders() { # offenders <root>
    git -C "$1" ls-files -z | tr '\0' '\n' | grep -E '(^|/)target/' || true
}

if [ "${1:-}" = --self-test ]; then
    # check.sh runs under hooks that export GIT_DIR and friends; a leaked one would point `git init` at the real repository.
    # shellcheck disable=SC1091 # the helper sits beside this script
    . "$(dirname "$0")/git-clean-env.sh"
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/no-target-dirs-test.XXXXXX")
    trap 'cd / && rm -r "$tmp"' EXIT
    git init -q "$tmp"
    mkdir -p "$tmp/a/src/target" "$tmp/a/src/targets" "$tmp/a/src/retarget" "$tmp/target"
    : >"$tmp/a/src/target.rs"
    : >"$tmp/a/src/retarget/x.rs"
    : >"$tmp/a/src/targets/x.rs"
    : >"$tmp/a/src/target/tests.rs"
    : >"$tmp/target/lib.rs"
    git -C "$tmp" add -A
    got=$(offenders "$tmp" | tr '\n' ' ')
    [ "$got" = "a/src/target/tests.rs target/lib.rs " ] || {
        echo "no-target-dirs-test: the self-test found '$got', expected 'a/src/target/tests.rs target/lib.rs '" >&2
        exit 1
    }
    git -C "$tmp" rm -q -r --cached a/src/target target
    [ -z "$(offenders "$tmp")" ] || {
        echo "no-target-dirs-test: the self-test flagged a file named target.rs or a directory named targets or retarget" >&2
        exit 1
    }
    exit 0
fi

root=${1:-$(cd "$here/.." && pwd)}
found=$(offenders "$root")
if [ -n "$found" ]; then
    echo "no-target-dirs-test: a tracked path has a directory named target:" >&2
    echo "$found" | sed 's/^/  /' >&2
    echo "  Tools that clean, copy or ignore a directory named target by name drop these files. Rename the module." >&2
    exit 1
fi
echo "no-target-dirs-test: no tracked directory is named target"
