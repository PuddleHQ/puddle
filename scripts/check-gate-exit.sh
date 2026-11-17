#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# A gate that ends the whole check run early. The serial prefix of scripts/check.sh runs a gate in the main shell, so an
# `exit` that can exit 0 inside `run_gate` ended the run right there as a pass and skipped every gate named after it
# (diff-coverage-fast did this from 2026-10-31 to 2026-11-16: 152 host pre-checks of 122 PRs wrote a passing marker without
# running clippy, coverage and the rest). A gate ends itself with `return`; `exit` is for a failure only. This lint fails
# an `exit` in the body of `run_gate` unless its status is a literal 1 to 9. A line that exits with a variable that is
# known non-zero says `exit-ok: <reason>`.
#   scripts/check-gate-exit.sh              scan scripts/check.sh
#   scripts/check-gate-exit.sh --self-test  prove the scan on throwaway scripts
set -euo pipefail

# Prints `file:line: text` for every offending `exit` in the `run_gate` function of the file named in $1.
scan() {
    awk -v file="$1" '
        /^run_gate\(\) *\{/ { in_fn = 1; next }
        in_fn && /^\}/ { in_fn = 0 }
        !in_fn { next }
        {
            line = $0
            sub(/^[ \t]+/, "", line)
            if (line ~ /^#/) next
            if (line ~ /exit-ok:/) next
            code = line
            sub(/[ \t]+#.*$/, "", code)
            while (match(code, /(^|[;&|({ \t])exit([ \t;&|)}]|$)[^;&|)}]*/)) {
                hit = substr(code, RSTART, RLENGTH)
                code = substr(code, RSTART + RLENGTH)
                sub(/^[^e]*/, "", hit)
                if (hit ~ /^exit[ \t]+[1-9][ \t]*$/) continue
                printf "%s:%d: %s\n", file, NR, line
                break
            }
        }
    ' "$1"
}

if [ "${1:-}" = --self-test ]; then
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/gate-exit.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    fail() { echo "check-gate-exit self-test: $*" >&2; exit 1; }
    # shellcheck disable=SC2016 # fixture text, not shell
    {
        printf '%s\n' 'run_gate() {' '    case "$1" in' '    a)' '        [ -n "$x" ] || exit 0' '        ;;' '    esac' '}' >"$tmp/bad1.sh"
        printf '%s\n' 'run_gate() {' '    case "$1" in' '    a)' '        exit' '        ;;' '    esac' '}' >"$tmp/bad2.sh"
        printf '%s\n' 'run_gate() {' '    case "$1" in' '    a) exit "$rc" ;;' '    esac' '}' >"$tmp/bad3.sh"
        printf '%s\n' 'run_gate() {' '    case "$1" in' '    a)' '        if x; then' '            exit 0' '        fi' '        ;;' '    esac' '}' >"$tmp/bad4.sh"
        printf '%s\n' 'run_gate() {' '    case "$1" in' '    a) x && exit 0 ;;' '    esac' '}' >"$tmp/bad5.sh"
        printf '%s\n' 'run_gate() {' '    case "$1" in' '    a)' '        [ -n "$x" ] || {' '            echo no >&2' '            exit 1' '        }' '        return 0' '        ;;' '    *)' '        exit 2' '        ;;' '    esac' '}' \
            'exit 0' >"$tmp/good1.sh"
        printf '%s\n' 'run_gate() {' '    # exit 0 in a comment is fine' '    case "$1" in' '    a) echo "exit 0 in text" ;;' '    b) exit "$rc" # exit-ok: rc is non-zero here' ';;' '    esac' '}' \
            'other() {' '    exit 0' '}' >"$tmp/good2.sh"
    }
    for b in bad1.sh bad2.sh bad3.sh bad4.sh bad5.sh; do
        [ -n "$(scan "$tmp/$b")" ] || fail "$b was not flagged"
    done
    for g in good1.sh good2.sh; do
        [ -z "$(scan "$tmp/$g")" ] || fail "$g was flagged: $(scan "$tmp/$g")"
    done
    echo "check-gate-exit self-test: ok"
    exit 0
fi

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
found=$(scan "$here/check.sh")
if [ -n "$found" ]; then
    printf '%s\n' "$found" >&2
    echo "check-gate-exit: an exit that can be 0 inside run_gate ends the whole run as a pass; use \`return\` (or add \`exit-ok: <reason>\` when the status is known non-zero)" >&2
    exit 1
fi
echo "check-gate-exit: ok"
