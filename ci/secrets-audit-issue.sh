#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Keeps the tracking issue of the nightly secrets audit of every ref (.github/workflows/secrets-audit.yml). Usage:
#   ci/secrets-audit-issue.sh <gitleaks report.json> <exit code of scripts/check-secrets.sh> <run url>
# Exit code 1 (a finding) opens the issue, or rewrites its body when it is already open; exit code 0 closes the
# open issue with a comment; any other code (gitleaks missing, a crash) touches nothing and fails, because an
# outage must not close an issue about a real finding. The issue text names file, line, rule and commit of each
# finding and never a value: only those four report fields are read, so neither the match nor the secret can
# reach the issue, redacted or not. The commit may sit on a branch that has no pull request, which is what this
# audit is for: the PR gate scans a pull request's own history only.
#
# Environment: GH_TOKEN (issues: write), GITHUB_REPOSITORY; SECRETS_AUDIT_GH replaces `gh` in the self-test
# (ci/secrets-audit-issue-test.sh).
set -uo pipefail

[ "$#" = 3 ] || { echo "usage: secrets-audit-issue.sh <report.json> <exit code> <run url>" >&2; exit 2; }
report=$1 code=$2 run_url=$3
gh=${SECRETS_AUDIT_GH:-gh}
title="Secrets audit: a finding in the repository history"

find_issue() {
    "$gh" issue list --state open --search "in:title \"$title\"" --json number,title --limit 20 |
        jq -r --arg t "$title" 'map(select(.title == $t)) | first | .number // empty'
}

# body <report.json> <run url>: the issue text.
body() {
    local n
    n=$(jq 'length' "$1") || return 1
    echo "The nightly audit (\`PUDDLE_SECRETS_SCOPE=all\` over a full clone, every ref) found $n finding(s). No value is shown here:"
    echo "open the commit and read the line."
    echo
    echo "| rule | file | line | commit |"
    echo "|---|---|---|---|"
    jq -r '.[] | [.RuleID, .File, .StartLine, .Commit[0:12]] | map(tostring | gsub("[|\\n\\r]"; " ")) | "| \(.[0]) | \(.[1]) | \(.[2]) | \(.[3]) |"' "$1" || return 1
    echo
    echo "Find the branches holding a commit with \`git branch -r --contains <commit>\`. A real secret: revoke it first, then"
    echo "remove it from history. A fake fixture: add it to \`.gitleaks.toml\` in the same commit as the token (docs/STANDARDS.md)."
    echo
    echo "Run: $2. The audit rewrites this issue every night and closes it on the first clean run."
}

case $code in
0)
    number=$(find_issue) || exit 1
    if [ -n "$number" ]; then
        "$gh" issue close "$number" --comment "No ref holds a finding in $run_url; closing." || exit 1
        echo "secrets-audit-issue: clean; closed #$number"
    else
        echo "secrets-audit-issue: clean; no open issue"
    fi
    exit 0
    ;;
1) ;;
*)
    echo "secrets-audit-issue: the gate exited $code; no issue touched" >&2
    exit 1
    ;;
esac

[ -s "$report" ] || { echo "secrets-audit-issue: no report at $report" >&2; exit 1; }
tmp=$(mktemp -d "${TMPDIR:-/tmp}/secrets-audit-issue.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
body "$report" "$run_url" >"$tmp/body.md" || { echo "secrets-audit-issue: the report is not readable" >&2; exit 1; }

number=$(find_issue) || exit 1
if [ -n "$number" ]; then
    "$gh" issue edit "$number" --body-file "$tmp/body.md" || exit 1
    echo "secrets-audit-issue: updated #$number"
else
    "$gh" issue create --title "$title" --body-file "$tmp/body.md" || exit 1
    echo "secrets-audit-issue: opened a new issue"
fi
