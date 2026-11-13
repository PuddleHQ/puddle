#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/dependabot-auto-merge.sh with a fake `gh` that serves a PR JSON ($FAKE_PR) and records each
# call: patch-only, patch-and-minor, mixed with a major, major, github-actions, fork pin, human commit, draft,
# wrong base and a missing metadata block. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/dependabot-auto-merge.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dependabot-auto-merge-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "dependabot-auto-merge-test: FAIL: $*" >&2; fail=1; }

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_LOG"
case "$1 $2" in
"pr view") [ "${FAKE_FAIL:-}" = 1 ] && exit 1; cat "$FAKE_PR" ;;
"pr merge") ;;
esac
GH
chmod +x "$tmp/bin/gh"

# msg <name:type>... -> a Dependabot commit message body with that updated-dependencies block
msg() {
    printf 'Bumps the group.\n\n---\nupdated-dependencies:\n'
    for e in "$@"; do
        printf -- '- dependency-name: %s\n  dependency-version: 1.2.3\n  dependency-type: direct:production\n  update-type: version-update:semver-%s\n' "${e%%:*}" "${e#*:}"
    done
    printf '...\n\nSigned-off-by: dependabot[bot] <support@github.com>'
}
# pr <branch> <author> <body> [state] [draft] [base]
pr() {
    jq -n --arg b "$1" --arg a "$2" --arg m "$3" --arg s "${4:-OPEN}" --argjson d "${5:-false}" --arg base "${6:-develop}" \
        '{state: $s, isDraft: $d, baseRefName: $base, headRefName: $b, headRefOid: "abc123",
          commits: [{messageHeadline: "bump", messageBody: $m, authors: [{login: $a}]}]}'
}

# run <label> <want armed 1|0> <want output fragment> <pr json file>
run() {
    local label=$1 armed=$2 want=$3 out rc
    : >"$tmp/log"
    out=$(FAKE_PR=$4 FAKE_LOG=$tmp/log PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r "$script" 7 2>&1)
    rc=$?
    [ "$rc" = 0 ] || bad "$label: exit $rc: $out"
    case $out in *"$want"*) ;; *) bad "$label: output lacks '$want': $out" ;; esac
    if [ "$armed" = 1 ]; then
        grep -qx 'pr merge 7 -R o/r --auto --squash --match-head-commit abc123' "$tmp/log" || bad "$label: not armed as expected: $(cat "$tmp/log")"
    else
        ! grep -q '^pr merge' "$tmp/log" || bad "$label: armed but should stay manual"
    fi
}

B=dependabot/cargo/develop/auto-abc
bot='dependabot[bot]'
pr $B "$bot" "$(msg tokio:patch serde:patch)" >"$tmp/a"
run "patch only" 1 "arming" "$tmp/a"
pr $B "$bot" "$(msg tokio:patch serde:minor)" >"$tmp/a"
run "patch and minor" 1 "2 patch or minor" "$tmp/a"
pr dependabot/npm_and_yarn/ui/develop/auto-x "$bot" "$(msg svelte:minor)" >"$tmp/a"
run "npm minor" 1 "arming" "$tmp/a"
pr $B "$bot" "$(msg tokio:patch zbus:major)" >"$tmp/a"
run "mixed with a major" 0 "zbus is 'version-update:semver-major'" "$tmp/a"
pr $B "$bot" "$(msg zbus:major)" >"$tmp/a"
run "major only" 0 "only patch and minor" "$tmp/a"
pr dependabot/github_actions/develop/actions-x "$bot" "$(msg actions/checkout:patch)" >"$tmp/a"
run "github-actions" 0 "not the auto group" "$tmp/a"
pr $B "$bot" "$(msg microsandbox:patch)" >"$tmp/a"
run "fork pin" 0 "fork pin" "$tmp/a"
pr $B "$bot" "$(msg msb_krun:minor)" >"$tmp/a"
run "fork pin (libkrun crate)" 0 "fork pin" "$tmp/a"
pr dependabot/cargo/develop/major-x "$bot" "$(msg tokio:patch)" >"$tmp/a"
run "major group branch" 0 "not the auto group" "$tmp/a"
pr dependabot/npm_and_yarn/ci/login-tools/develop/login-tools-x "$bot" "$(msg claude-code:patch)" >"$tmp/a"
run "login-tools" 0 "not the auto group" "$tmp/a"
pr dependabot/npm_and_yarn/crates/puddle-api/openapi/develop/openapi-codegen-auto-x "$bot" "$(msg openapi-typescript:minor)" >"$tmp/a"
run "openapi auto group" 1 "arming" "$tmp/a"
pr $B alice "$(msg tokio:patch)" >"$tmp/a"
run "human commit" 0 "not authored by" "$tmp/a"
pr $B "$bot" "$(msg tokio:patch)" OPEN true >"$tmp/a"
run "draft" 0 "not an open, ready PR" "$tmp/a"
pr $B "$bot" "$(msg tokio:patch)" MERGED >"$tmp/a"
run "merged" 0 "not an open, ready PR" "$tmp/a"
pr $B "$bot" "$(msg tokio:patch)" OPEN false main >"$tmp/a"
run "base main" 0 "base is not develop" "$tmp/a"
pr $B "$bot" "Bumps tokio." >"$tmp/a"
run "no metadata block" 0 "no updated-dependencies" "$tmp/a"
pr $B "$bot" "$(msg tokio:weird)" >"$tmp/a"
run "unknown type" 0 "only patch and minor" "$tmp/a"
# a gh failure is an error, not a silent skip
: >"$tmp/log"
if FAKE_FAIL=1 FAKE_PR=$tmp/a FAKE_LOG=$tmp/log PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r "$script" 7 >/dev/null 2>&1; then bad "gh failure exited 0"; fi

if [ "$fail" = 0 ]; then
    echo "dependabot-auto-merge-test: ok"
else
    exit 1
fi
