#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Downloads the MSI an upgrade starts from: what a user has installed before this build.
#
#   ci/fetch-baseline-msi.sh <out-dir>
#
# That is the MSI of the newest published release. While there is no release yet, it is the MSI
# the newest green msi workflow run on develop kept (the `puddle-msi` artifact, 14 days), from a
# commit other than this one, so the upgrade still crosses commits. Prints the MSI's path on the
# last line of stdout and says where it came from on stderr; with GITHUB_STEP_SUMMARY set, the
# source also goes into the run's summary. Exits 1 when there is no baseline (a release without
# an MSI, or no kept develop MSI), naming why. Needs GH_TOKEN, GITHUB_REPOSITORY, GITHUB_SHA and
# `actions: read`. Self-test: ci/fetch-baseline-msi-test.sh.
set -euo pipefail

out=${1:?usage: fetch-baseline-msi.sh <out-dir>}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}
sha=${GITHUB_SHA:?GITHUB_SHA is not set}
# Kept develop MSIs looked at, newest first, for one whose run is green.
max_candidates=${BASELINE_MAX_CANDIDATES:-10}
pattern='Puddle_*_x64_en-US.msi'

die() { echo "fetch-baseline-msi: $*" >&2; exit 1; }

mkdir -p "$out"
[ -z "$(ls -A "$out")" ] || die "$out is not empty"

tag=$(gh release list -R "$repo" --exclude-drafts --limit 1 --json tagName --jq '.[0].tagName // ""') ||
    die "could not list the releases of $repo"
if [ -n "$tag" ]; then
    gh release download "$tag" -R "$repo" --pattern "$pattern" --dir "$out" ||
        die "release $tag has no MSI ($pattern): an upgrade from it cannot be tested"
    source="release $tag"
else
    candidates=$(gh api "repos/$repo/actions/artifacts?name=puddle-msi&per_page=100" --jq \
        ".artifacts[] | select(.expired | not) | select(.workflow_run.head_branch == \"develop\")
         | select(.workflow_run.head_sha != \"$sha\") | \"\(.workflow_run.id) \(.workflow_run.head_sha)\"") ||
        die "could not list the kept MSIs of $repo"
    run="" from=""
    while read -r id head; do
        [ -n "$id" ] || continue
        if [ "$(gh run view "$id" -R "$repo" --json conclusion --jq .conclusion)" = success ]; then
            run=$id from=$head
            break
        fi
    done < <(printf '%s\n' "$candidates" | head -n "$max_candidates")
    [ -n "$run" ] || die "$repo has no release and no green develop run of msi.yml that kept an MSI (puddle-msi, kept 14 days) on another commit"
    gh run download "$run" -R "$repo" --name puddle-msi --dir "$out" || die "could not download the MSI of run $run"
    source="develop at ${from:0:9} (msi run $run; no release yet)"
fi

shopt -s nullglob
found=("$out"/Puddle_*_x64_en-US.msi)
[ "${#found[@]}" = 1 ] || die "expected one $pattern from $source in $out, found: $(ls -A "$out")"
echo "baseline: ${found[0]##*/} from $source" >&2
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "### upgrade baseline: ${found[0]##*/} from $source" >>"$GITHUB_STEP_SUMMARY"
fi
echo "${found[0]}"
