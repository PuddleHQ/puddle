#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/secrets-audit-issue.sh with a stub `gh` (records its write calls, answers `issue list` from a
# variable, keeps the issue body). Cases: a finding with no open issue opens one whose body names rule, file,
# line and a shortened commit and carries no value (the report holds a Secret and a Match; neither may appear);
# a finding with an open issue rewrites it; a clean run closes it and leaves other issues alone; a crashed gate
# (exit 2), a missing report and a malformed report touch nothing and fail; a bad argument count exits 2.
# Not seen: the real `gh`, and gitleaks' own report (it writes these field names).
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/secrets-audit-issue-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() {
    echo "secrets-audit-issue-test: $*" >&2
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
cat >"$tmp/report.json" <<'JSON'
[
  {"RuleID":"github-pat","File":"crates/a/src/lib.rs","StartLine":12,"Commit":"0123456789abcdef0123456789abcdef01234567","Secret":"REDACTED-SECRET-VALUE","Match":"token = \"REDACTED-MATCH-VALUE\"","Author":"someone","Email":"someone@example.test"},
  {"RuleID":"generic|api-key","File":"docs/odd|name.md","StartLine":3,"Commit":"fedcba9876543210fedcba9876543210fedcba98","Secret":"REDACTED-SECRET-VALUE","Match":"x"}
]
JSON
echo 'not json' >"$tmp/bad.json"
open='[{"number":7,"title":"Secrets audit: a finding in the repository history"},{"number":9,"title":"Other"}]'

# run <name> <wanted exit> <report path> <code> <open issues json>
run() {
    local name=$1 want=$2 report=$3 code=$4 issues=$5 got=0
    : >"$tmp/gh.log"
    : >"$tmp/body.md"
    OPEN_ISSUES=$issues GH_LOG="$tmp/gh.log" BODY_COPY="$tmp/body.md" SECRETS_AUDIT_GH="$tmp/gh" \
        "$here/secrets-audit-issue.sh" "$report" "$code" https://example.test/run/42 >"$tmp/out" 2>&1 || got=$?
    [ "$got" = "$want" ] || fail "$name: exit $got, wanted $want: $(cat "$tmp/out")"
}
writes() { grep -c "^issue " "$tmp/gh.log" || true; }

run "finding, no open issue" 0 "$tmp/report.json" 1 '[]'
grep -q '^issue create --title Secrets audit: a finding in the repository history ' "$tmp/gh.log" || fail "no issue created: $(cat "$tmp/gh.log")"
grep -q '^| github-pat | crates/a/src/lib.rs | 12 | 0123456789ab |$' "$tmp/body.md" || fail "row missing: $(cat "$tmp/body.md")"
grep -q '^| generic api-key | docs/odd name.md | 3 | fedcba987654 |$' "$tmp/body.md" || fail "a pipe in a field must not break the table: $(cat "$tmp/body.md")"
grep -q '2 finding' "$tmp/body.md" || fail "count missing"
grep -q 'https://example.test/run/42' "$tmp/body.md" || fail "run link missing"
if grep -q 'REDACTED\|someone' "$tmp/body.md"; then fail "the body carries a value or an author: $(cat "$tmp/body.md")"; fi
run "finding, issue open" 0 "$tmp/report.json" 1 "$open"
{ grep -q '^issue edit 7 ' "$tmp/gh.log" && [ "$(writes)" = 1 ]; } || fail "must edit #7 only: $(cat "$tmp/gh.log")"
run "clean closes the open issue" 0 "$tmp/none.json" 0 "$open"
{ grep -q '^issue close 7 ' "$tmp/gh.log" && [ "$(writes)" = 1 ]; } || fail "clean must close #7 only: $(cat "$tmp/gh.log")"
run "clean, nothing open" 0 "$tmp/none.json" 0 '[{"number":9,"title":"Other"}]'
[ "$(writes)" = 0 ] || fail "clean with no audit issue must write nothing"
run "gate crashed" 1 "$tmp/report.json" 2 "$open"
[ "$(writes)" = 0 ] || fail "a crash must not touch an issue"
run "missing report" 1 "$tmp/none.json" 1 '[]'
[ "$(writes)" = 0 ] || fail "a missing report must not open an issue"
run "malformed report" 1 "$tmp/bad.json" 1 '[]'
[ "$(writes)" = 0 ] || fail "a malformed report must not open an issue"
got=0
"$here/secrets-audit-issue.sh" only-one >"$tmp/out" 2>&1 || got=$?
[ "$got" = 2 ] || fail "wrong argument count must exit 2, got $got"
echo "secrets-audit-issue-test: ok"
