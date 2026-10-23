#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/advisories-issue.sh with a stub check command (for `rust`: prints a report shaped like cargo-deny
# 0.20's JSON on stderr; for `npm`: prints what `node ui/scripts/audit.ts --json` prints on stdout; each exits with
# a chosen status) and a stub `gh` (records its write calls, answers `issue list` from a
# variable). Cases: an advisory with no open issue opens one; with an open issue rewrites it; a clean run
# closes the open issue and leaves none alone (the same cases for the npm source, whose issue has its own title
# and which never touches the Rust issue); a run that fails without naming an advisory (outage) neither
# closes nor opens anything and fails; an ignored advisory (a note) counts as clean. Not seen: the real
# `gh` and the GitHub API, and the real cargo-deny report beyond the fields the script reads.
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/advisories-issue-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "advisories-issue-test: $*" >&2
    exit 1
}

cat >"$tmp/deny" <<'STUB'
#!/bin/sh
if [ -n "${REPORT:-}" ]; then
    if [ "${REPORT_TO:-stderr}" = stdout ]; then cat "$REPORT"; else cat "$REPORT" >&2; fi
fi
exit "${DENY_STATUS:-0}"
STUB
# Write calls are logged; `issue create` and `issue edit` also keep a copy of the body file, which the script
# deletes with its temp dir.
cat >"$tmp/gh" <<'STUB'
#!/bin/sh
case "$1 $2" in
"issue list") printf '%s\n' "${OPEN_ISSUES:-[]}" ;;
"issue create") echo "$*" >>"$GH_LOG"; cp "$6" "$tmp_body" ;;
"issue edit") echo "$*" >>"$GH_LOG"; cp "$5" "$tmp_body" ;;
*) echo "$*" >>"$GH_LOG" ;;
esac
STUB
chmod +x "$tmp/deny" "$tmp/gh"

cat >"$tmp/vulnerable.json" <<'JSON'
{"fields":{"advisory":{"id":"RUSTSEC-2026-0386","package":"tokio","title":"`AsyncFd::new` and related constructors violate I/O safety"},"code":"unsound","graphs":[{"Krate":{"name":"tokio","version":"1.53.2"},"parents":[]}],"labels":[],"message":"`AsyncFd::new` violates I/O safety","notes":["ID: RUSTSEC-2026-0386","Solution: Upgrade to >=1.51.6, <1.52.0 OR >=1.53.3 (try `cargo update -p tokio`)"],"severity":"error"},"type":"diagnostic"}
{"fields":{"advisories":{"errors":1,"helps":0,"notes":0,"warnings":0}},"type":"summary"}
JSON
cat >"$tmp/ignored.json" <<'JSON'
{"fields":{"advisory":{"id":"RUSTSEC-2025-0141","package":"bincode","title":"bincode is unmaintained"},"code":"unmaintained","graphs":[{"Krate":{"name":"bincode","version":"2.0.1"},"parents":[]}],"message":"ignored","notes":[],"severity":"note"},"type":"diagnostic"}
{"fields":{"advisories":{"errors":0,"helps":0,"notes":1,"warnings":0}},"type":"summary"}
JSON
cat >"$tmp/open.json" <<'JSON'
[{"number":7,"title":"Dependency advisories: cargo deny check advisories fails"},{"number":9,"title":"Dependency advisories: something else"}]
JSON

# run <name> <expected exit> <report|-> <check status> <open issues json> [source]; the gh write calls land in $tmp/gh.log
run() {
    local name=$1 want=$2 report=$3 status=$4 open=$5 source=${6:-rust} to=stderr got=0
    [ "$source" = npm ] && to=stdout
    : >"$tmp/gh.log"
    REPORT=$([ "$report" = - ] || echo "$tmp/$report") REPORT_TO=$to DENY_STATUS=$status OPEN_ISSUES=$open GH_LOG="$tmp/gh.log" tmp_body="$tmp/body.md" \
        ADVISORIES_CHECK="$tmp/deny" ADVISORIES_GH="$tmp/gh" GITHUB_REPOSITORY=o/r GITHUB_RUN_ID=42 \
        "$here/advisories-issue.sh" "$source" >"$tmp/out" 2>&1 || got=$?
    [ "$got" = "$want" ] || fail "$name: exit $got, wanted $want: $(cat "$tmp/out")"
}

open=$(cat "$tmp/open.json")
run "advisory, no open issue" 1 vulnerable.json 1 '[]'
grep -q '^issue create --title Dependency advisories: cargo deny check advisories fails --body-file ' "$tmp/gh.log" || fail "no issue opened: $(cat "$tmp/gh.log")"
[ "$(wc -l <"$tmp/gh.log")" = 1 ] || fail "more than one write: $(cat "$tmp/gh.log")"
grep -q 'RUSTSEC-2026-0386' "$tmp/body.md" || fail "body lacks the advisory id"
grep -q 'tokio 1.53.2' "$tmp/body.md" || fail "body lacks the crate and version"
grep -q 'OR >=1.53.3' "$tmp/body.md" || fail "body lacks the fix hint"
grep -q 'actions/runs/42' "$tmp/body.md" || fail "body lacks the run link"

run "advisory, issue open" 1 vulnerable.json 1 "$open"
grep -q '^issue edit 7 --body-file ' "$tmp/gh.log" || fail "open issue not rewritten: $(cat "$tmp/gh.log")"
grep -q 'issue create' "$tmp/gh.log" && fail "a second issue was opened"

run "clean, issue open" 0 - 0 "$open"
grep -q '^issue close 7 --comment ' "$tmp/gh.log" || fail "open issue not closed: $(cat "$tmp/gh.log")"
grep -q 'issue close 9' "$tmp/gh.log" && fail "another issue was closed"

run "clean, no issue" 0 - 0 '[]'
[ ! -s "$tmp/gh.log" ] || fail "a clean run with no issue wrote: $(cat "$tmp/gh.log")"

run "ignored advisory is clean" 0 ignored.json 0 "$open"
grep -q '^issue close 7' "$tmp/gh.log" || fail "an ignored advisory kept the issue open"

run "outage touches nothing" 1 - 1 "$open"
[ ! -s "$tmp/gh.log" ] || fail "an outage wrote: $(cat "$tmp/gh.log")"

# The npm source: audit.ts --json prints the failing advisories as an array on stdout (exit 1), `[]` when clean
# (exit 0), nothing and exit 2 when npm could not answer.
cat >"$tmp/npm-vulnerable.json" <<'JSON'
[{"id":"GHSA-35jh-r3h4-6jhm","name":"lodash","severity":"high","title":"Command Injection in lodash"}]
JSON
echo '[]' >"$tmp/npm-clean.json"
npm_title="Dependency advisories: npm audit fails"
npm_open="[{\"number\":7,\"title\":\"Dependency advisories: cargo deny check advisories fails\"},{\"number\":11,\"title\":\"$npm_title\"}]"

run "npm advisory, no npm issue open (the Rust issue is not it)" 1 npm-vulnerable.json 1 "$open" npm
grep -q "^issue create --title $npm_title --body-file " "$tmp/gh.log" || fail "no npm issue opened: $(cat "$tmp/gh.log")"
grep -q 'GHSA-35jh-r3h4-6jhm' "$tmp/body.md" || fail "npm body lacks the advisory id"
grep -q 'github.com/advisories/GHSA-35jh-r3h4-6jhm' "$tmp/body.md" || fail "npm body lacks the advisory link"
grep -q 'audit-exceptions.json' "$tmp/body.md" || fail "npm body lacks the exception file"
grep -q 'advisories-npm' "$tmp/body.md" || fail "npm body lacks the job name"

run "npm advisory, npm issue open" 1 npm-vulnerable.json 1 "$npm_open" npm
grep -q '^issue edit 11 --body-file ' "$tmp/gh.log" || fail "npm issue not rewritten: $(cat "$tmp/gh.log")"
grep -q 'issue edit 7' "$tmp/gh.log" && fail "the Rust issue was rewritten by the npm run"

run "npm clean, npm issue open" 0 npm-clean.json 0 "$npm_open" npm
grep -q '^issue close 11 --comment ' "$tmp/gh.log" || fail "npm issue not closed: $(cat "$tmp/gh.log")"
grep -q 'issue close 7' "$tmp/gh.log" && fail "the Rust issue was closed by the npm run"

run "rust clean leaves the npm issue" 0 - 0 "$npm_open"
grep -q 'issue close 11' "$tmp/gh.log" && fail "the npm issue was closed by the Rust run"

run "npm clean, no issue" 0 npm-clean.json 0 '[]' npm
[ ! -s "$tmp/gh.log" ] || fail "a clean npm run with no issue wrote: $(cat "$tmp/gh.log")"

run "npm outage touches nothing" 1 - 2 "$npm_open" npm
[ ! -s "$tmp/gh.log" ] || fail "an npm outage wrote: $(cat "$tmp/gh.log")"

run "npm findings that cannot be parsed touch nothing" 1 ignored.json 1 "$npm_open" npm
[ ! -s "$tmp/gh.log" ] || fail "an unreadable npm report wrote: $(cat "$tmp/gh.log")"

"$here/advisories-issue.sh" >/dev/null 2>&1 && fail "no source argument succeeded"

echo "advisories-issue-test: ok"
