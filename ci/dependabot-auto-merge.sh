#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Arms auto-merge (squash, bound to the head commit) on a Dependabot pull request whose every update is a
# patch or a minor one, so it lands through the normal required checks and the merge queue like any
# other pull request. Reads only data about the PR, never its code, so it is safe under
# `pull_request_target` (.github/workflows/dependabot-auto-merge.yml).
#
# Arms only when ALL hold:
#   - the PR is open, not a draft, and targets develop;
#   - every commit on it is authored by dependabot[bot] (a human push to the branch keeps it manual);
#   - the branch is the `auto` group's of the cargo, ui or openapi entry in .github/dependabot.yml (the `major`
#     groups, github-actions, login-tools and the lab's uv entry stay manual);
#   - the commit message's `updated-dependencies:` block (the one dependabot/fetch-metadata reads) lists
#     at least one update and every `update-type` is version-update:semver-patch or ...-minor, so a major
#     or an unknown type refuses;
#   - no updated dependency is a fork pin (microsandbox*, msb_krun*, libkrun*, russh*, imago*): those move by hand.
# Otherwise it prints why and exits 0 (a PR left manual is not an error). A failing `gh` exits non-zero.
# Usage: ci/dependabot-auto-merge.sh <pr number>   Needs GH_TOKEN, GITHUB_REPOSITORY. Self-test: ci/dependabot-auto-merge-test.sh.
set -euo pipefail

pr=${1:?usage: dependabot-auto-merge.sh <pr number>}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY not set}
bot='dependabot[bot]'

info=$(gh pr view "$pr" -R "$repo" --json state,isDraft,baseRefName,headRefName,headRefOid,commits)
skip() { echo "dependabot-auto-merge: PR #$pr stays manual: $*"; exit 0; }

jq -e '.state == "OPEN" and (.isDraft | not)' <<<"$info" >/dev/null || skip "not an open, ready PR"
[ "$(jq -r .baseRefName <<<"$info")" = develop ] || skip "base is not develop"
branch=$(jq -r .headRefName <<<"$info")
case $branch in
dependabot/cargo/develop/auto-* | dependabot/npm_and_yarn/ui/develop/auto-* | dependabot/npm_and_yarn/crates/puddle-api/openapi/develop/openapi-codegen-auto-*) ;;
*) skip "branch $branch is not the auto group of a cargo or npm entry" ;;
esac
jq -e --arg bot "$bot" '(.commits | length) > 0 and all(.commits[]; (.authors | length) > 0 and all(.authors[]; .login == $bot))' <<<"$info" >/dev/null ||
    skip "a commit on the branch is not authored by $bot"

# The `updated-dependencies:` block of every commit message: "- dependency-name: x" opens an entry, "update-type: y" closes it.
updates=$(jq -r '.commits[] | .messageBody' <<<"$info" | awk '
    /^updated-dependencies:/ { on = 1; next }
    /^\.\.\.$/ { on = 0 }
    on && /^- dependency-name: / { sub(/^- dependency-name: */, ""); name = $0; next }
    on && /^  update-type: / { sub(/^  update-type: */, ""); print name "\t" $0 }')
[ -n "$updates" ] || skip "no updated-dependencies block in the commit message"

while IFS=$'\t' read -r name type; do
    case $type in
    version-update:semver-patch | version-update:semver-minor) ;;
    *) skip "$name is '$type' (only patch and minor arm)" ;;
    esac
    case $name in
    microsandbox* | msb_krun* | libkrun* | russh* | imago*) skip "$name is a fork pin" ;;
    esac
done <<<"$updates"

sha=$(jq -r .headRefOid <<<"$info")
echo "dependabot-auto-merge: PR #$pr: $(wc -l <<<"$updates") patch or minor update(s); arming auto-merge on $sha"
gh pr merge "$pr" -R "$repo" --auto --squash --match-head-commit "$sha"
