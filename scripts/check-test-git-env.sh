#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# A test that runs git must not inherit the repository variables of a calling git hook: a `git init`
# or `git clone --bare` under an exported GIT_DIR acts on the hook's repository (from a linked
# worktree it turned the shared checkout bare, and a push once reached the real remote).
#   scripts/check-test-git-env.sh              checks every tracked shell test and Rust test harness
#   scripts/check-test-git-env.sh FILE...      checks those files
#   scripts/check-test-git-env.sh --self-test  proves it fails on a planted offender and passes a fixed one
# A shell test (`*test*.sh` in ci/ and scripts/) that calls git passes when it sources
# scripts/git-clean-env.sh, unsets `git rev-parse --local-env-vars`, or unsets GIT_DIR,
# GIT_INDEX_FILE and GIT_WORK_TREE by name. A Rust file under crates/*/tests (or the puddle-e2e
# harness) that runs `Command::new("git")` passes when it also removes the variables (`env_remove`,
# `env_clear`) or goes through a helper that does (`clean_git`, `git_command`, `command_without`).
# shellcheck disable=SC2016 # the self-test's planted scripts are literal text
set -eu
cd "$(dirname "$0")/.."

code() { grep -v '^[[:space:]]*\(#\|//\)' "$1" || true; }

shell_offends() {
    code "$1" | grep -Eq '(^|[[:space:];&|(`]|\$\()git[[:space:]]' || return 1
    c=$(code "$1")
    printf '%s\n' "$c" | grep -Eq 'git-clean-env\.sh|rev-parse --local-env-vars' && return 1
    u=$(printf '%s\n' "$c" | grep -E '^[[:space:]]*unset[[:space:]]' || true)
    for v in GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE; do
        printf '%s\n' "$u" | grep -q "$v" || return 0
    done
    return 1
}

rust_offends() {
    code "$1" | grep -q 'Command::new("git")' || return 1
    code "$1" | grep -Eq 'env_remove|env_clear|clean_git|git_command|command_without' && return 1
    return 0
}

check() {
    bad=0
    for f in "$@"; do
        case "$f" in
        *.sh) shell_offends "$f" && { echo "check-test-git-env: $f runs git without clearing the hook's GIT_* variables; add '. \"\$(dirname \"\$0\")/git-clean-env.sh\"' (scripts/git-clean-env.sh)" >&2; bad=1; } ;;
        *.rs) rust_offends "$f" && { echo "check-test-git-env: $f runs Command::new(\"git\") without env_remove of the hook's GIT_* variables (see clean_git in puddle-e2e)" >&2; bad=1; } ;;
        esac
    done
    return $bad
}

if [ "${1:-}" = "--self-test" ]; then
    t=$(mktemp -d)
    trap 'rm -rf "$t"' EXIT INT TERM
    printf '#!/bin/sh\ntmp=$(mktemp -d)\ngit init -q "$tmp/r"\n' >"$t/bad-test.sh"
    printf '#!/bin/sh\n. "$(dirname "$0")/git-clean-env.sh"\ngit init -q r\n' >"$t/sourced-test.sh"
    printf '#!/bin/sh\nunset $(git rev-parse --local-env-vars)\ngit init -q r\n' >"$t/rev-parse-test.sh"
    printf '#!/bin/sh\nunset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE\ngit init -q r\n' >"$t/named-test.sh"
    printf '#!/bin/sh\nunset GIT_DIR\ngit init -q r\n' >"$t/partial-test.sh"
    printf '#!/bin/sh\n# git init is only named in a comment\necho hi\n' >"$t/comment-test.sh"
    printf 'fn g() { Command::new("git").args(["init"]); }\n' >"$t/bad.rs"
    printf 'fn g() { Command::new("git").env_remove("GIT_DIR"); }\n' >"$t/good.rs"
    fail=0
    for f in bad-test.sh partial-test.sh bad.rs; do
        check "$t/$f" 2>/dev/null && { echo "self-test: $f should fail" >&2; fail=1; }
    done
    for f in sourced-test.sh rev-parse-test.sh named-test.sh comment-test.sh good.rs; do
        check "$t/$f" 2>/dev/null || { echo "self-test: $f should pass" >&2; fail=1; }
    done
    [ "$fail" = 0 ] && echo "check-test-git-env self-test ok"
    exit "$fail"
fi

if [ "$#" -gt 0 ]; then
    check "$@"
else
    # shellcheck disable=SC2046 # tracked paths hold no spaces
    check $(git ls-files -- 'ci/*test*.sh' 'scripts/*test*.sh' 'crates/*/tests/*.rs' 'crates/*/tests/**/*.rs' 'crates/puddle-e2e/src/*.rs' 'crates/puddle-e2e/src/**/*.rs')
fi
