#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Under `set -o pipefail` a pipe into an early-exit grep (`grep -q`, `grep -m`) fails at random: grep quits at its
# first match, the writer then gets SIGPIPE, the pipeline status becomes 141 and the match counts as a miss
# (measured 0.11 % under CPU load, exit 141). This gate fails that shape in every tracked shell script that sets
# pipefail and in every workflow file. Use a here-string (`grep -q x <<<"$(cmd)"`), `case`, or `[[ == ]]` instead.
# A line that really needs the pipe says `sigpipe-ok: <reason>`. Scripts without pipefail are not affected (the
# status of a pipe is then the status of its last command), so they are not scanned.
#   scripts/check-early-exit-pipes.sh              scan the repository
#   scripts/check-early-exit-pipes.sh --self-test  prove the scan on throwaway files
set -euo pipefail

# A pipe (not `||`) into grep, egrep, fgrep or rg with a flag cluster holding q or m, or a long --quiet/--max-count.
shape='(^|[^|])\| *(grep|egrep|fgrep|rg) +(-[A-Za-z]*[qm][A-Za-z]*|--quiet|--max-count)'

# Prints `file:line: text` for every offending line of the files named on stdin, one path per line.
scan() {
    local f
    while IFS= read -r f; do
        [ -f "$f" ] || continue
        case $f in */check-early-exit-pipes.sh) continue ;; esac # its self-test fixtures hold the shape on purpose
        case $f in
        *.yml | *.yaml) ;;
        *) grep -q 'pipefail' "$f" || continue ;;
        esac
        grep -nE -e "$shape" "$f" | grep -vE '^[0-9]+:[[:space:]]*#' | grep -v 'sigpipe-ok:' | sed "s|^|$f:|" || true
    done
}

if [ "${1:-}" = --self-test ]; then
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/early-exit-pipes.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    fail() { echo "check-early-exit-pipes self-test: $*" >&2; exit 1; }
    printf '%s\n' '#!/bin/bash' 'set -euo pipefail' 'x=1' 'cmd | grep -q a' >"$tmp/bad1.sh"
    printf '%s\n' '#!/bin/bash' 'set -eo pipefail' 'cmd | grep -Eqx a' >"$tmp/bad2.sh"
    printf '%s\n' '#!/bin/bash' 'set -eo pipefail' 'cmd | grep -m1 a' >"$tmp/bad3.sh"
    printf '%s\n' '      - run: |' '          cmd | grep -q a' >"$tmp/bad4.yml"
    # shellcheck disable=SC2016 # fixture text, not shell
    printf '%s\n' '#!/bin/bash' 'set -euo pipefail' 'grep -q a <<<"$(cmd)"' 'a || grep -q b f' 'cmd | grep -c a' \
        '# cmd | grep -q a' 'cmd | grep -q a # sigpipe-ok: tiny input' >"$tmp/good1.sh"
    printf '%s\n' '#!/bin/sh' 'cmd | grep -q a' >"$tmp/good2.sh"
    for b in bad1.sh bad2.sh bad3.sh bad4.yml; do
        [ -n "$(echo "$tmp/$b" | scan)" ] || fail "$b was not flagged"
    done
    for g in good1.sh good2.sh; do
        [ -z "$(echo "$tmp/$g" | scan)" ] || fail "$g was flagged"
    done
    echo "check-early-exit-pipes self-test: ok"
    exit 0
fi

cd "$(dirname "$0")/.."
hits=$(git ls-files -- '*.sh' '.github/workflows/*.yml' '.github/workflows/*.yaml' '.githooks/*' | scan)
if [ -n "$hits" ]; then
    echo "pipe into an early-exit grep (SIGPIPE under pipefail); capture to a variable and use <<<, case or [[ ]]:" >&2
    echo "$hits" >&2
    exit 1
fi
echo "early-exit pipes ok"
