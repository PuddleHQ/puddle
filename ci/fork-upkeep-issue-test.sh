#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/fork-upkeep-issue.sh with a stub `gh` (records its write calls, answers `issue list` from a
# variable). Cases: findings with no open issue open one whose body is the report plus the run link; findings
# with an open issue rewrite its body and open no second one; a clean report closes the open issue and leaves
# none alone; a crashed report (exit 1) and a missing report touch nothing and fail; a bad argument count exits 2.
# Not seen: the real `gh` and the GitHub API.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fork-upkeep-issue-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "fork-upkeep-issue-test: $*" >&2
    exit 1
}

cat >"$tmp/gh" <<'STUB'
#!/bin/sh
case "$1 $2" in
"issue list") printf '%s\n' "${OPEN_ISSUES:-[]}" ;;
"issue create") echo "$*" >>"$GH_LOG"; cp "$6" "$BODY_COPY" ;;
"issue edit") echo "$*" >>"$GH_LOG"; cp "$5" "$BODY_COPY" ;;
*) echo "$*" >>"$GH_LOG" ;;
esac
STUB
chmod +x "$tmp/gh"
# shellcheck disable=SC2016 # the backticks are report text, not a command
printf '# Fork upkeep\n\n- libkrun: `fix(vsock): x` (candidate) conflicts\n' >"$tmp/report.md"
open='[{"number":7,"title":"Fork upkeep: the forks do not replay on upstream"},{"number":9,"title":"Other"}]'

# run <name> <wanted exit> <report path> <code> <open issues json>
run() {
    local name=$1 want=$2 report=$3 code=$4 issues=$5 got=0
    : >"$tmp/gh.log"
    : >"$tmp/body.md"
    OPEN_ISSUES=$issues GH_LOG="$tmp/gh.log" BODY_COPY="$tmp/body.md" FORK_UPKEEP_GH="$tmp/gh" \
        "$here/fork-upkeep-issue.sh" "$report" "$code" https://example.test/run/42 >"$tmp/out" 2>&1 || got=$?
    [ "$got" = "$want" ] || fail "$name: exit $got, wanted $want: $(cat "$tmp/out")"
}

run "findings, no open issue" 0 "$tmp/report.md" 2 '[]'
grep -q '^issue create --title Fork upkeep: the forks do not replay on upstream --body-file ' "$tmp/gh.log" || fail "create: $(cat "$tmp/gh.log")"
grep -q 'conflicts' "$tmp/body.md" || fail "body has no report: $(cat "$tmp/body.md")"
grep -q 'Run: https://example.test/run/42' "$tmp/body.md" || fail "body has no run link: $(cat "$tmp/body.md")"

run "findings, issue open" 0 "$tmp/report.md" 2 "$open"
grep -q '^issue edit 7 --body-file ' "$tmp/gh.log" || fail "edit: $(cat "$tmp/gh.log")"
if grep -q 'issue create' "$tmp/gh.log"; then fail "an open issue got a second one"; fi

run "clean, issue open" 0 "$tmp/report.md" 0 "$open"
grep -q '^issue close 7 --comment ' "$tmp/gh.log" || fail "close: $(cat "$tmp/gh.log")"

run "clean, no issue" 0 "$tmp/report.md" 0 '[]'
[ ! -s "$tmp/gh.log" ] || fail "clean with no issue wrote: $(cat "$tmp/gh.log")"

run "crash" 1 "$tmp/report.md" 1 "$open"
[ ! -s "$tmp/gh.log" ] || fail "a crash touched an issue: $(cat "$tmp/gh.log")"

run "findings without a report" 1 "$tmp/missing.md" 2 '[]'
[ ! -s "$tmp/gh.log" ] || fail "no report touched an issue: $(cat "$tmp/gh.log")"

got=0
"$here/fork-upkeep-issue.sh" only-one >"$tmp/out" 2>&1 || got=$?
[ "$got" = 2 ] || fail "a bad argument count exited $got"
echo "fork-upkeep-issue-test: ok"
