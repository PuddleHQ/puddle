#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Downloads the MSI an upgrade starts from: what a user has installed before this build.
#
#   ci/fetch-baseline-msi.sh <out-dir> [<release tag>]
#   ci/fetch-baseline-msi.sh --list
#
# With no tag that is the MSI of the newest published release; with a tag, the MSI of that
# release (a user may skip versions, so the upgrade job runs once for every published release).
# `--list` prints the tags of all published releases as a JSON array for a job matrix, newest
# first (`[""]` while there is none: one job, which takes the develop fallback). While there is no
# release yet, the baseline is the MSI
# the newest msi workflow run on develop whose `msi` job passed kept (the `puddle-msi` artifact, 14
# days; the run's other jobs may be red, the upgrade job among them), from a commit other than this one, so the upgrade still crosses commits,
# and from a run that started before this one: the package version is 0.0.<run number>, so a develop run
# that started later holds a higher version, and Windows Installer refuses to upgrade down to this build (error 1603). Prints the MSI's path on the
# last line of stdout and says where it came from on stderr; with GITHUB_STEP_SUMMARY set, the
# source also goes into the run's summary. Exits 1 when there is no baseline (a release without
# an MSI, or no kept develop MSI), naming why. Needs GH_TOKEN, GITHUB_REPOSITORY, GITHUB_SHA, GITHUB_RUN_ID and
# `actions: read`. Self-test: ci/fetch-baseline-msi-test.sh.
#
# BASELINE_RUN=<run id> takes the MSI that run of msi.yml kept instead (the dispatch input
# `baseline_run`): an upgrade from an older build than the newest, such as one from before a change
# to the guest's layout.
set -euo pipefail

repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}
die() { echo "fetch-baseline-msi: $*" >&2; exit 1; }

if [ "${1:-}" = --list ]; then
    tags=$(gh release list -R "$repo" --exclude-drafts --limit 100 --json tagName --jq '[.[].tagName]') ||
        die "could not list the releases of $repo"
    if [ "$tags" = '[]' ]; then echo '[""]'; else echo "$tags"; fi
    exit 0
fi

out=${1:?usage: fetch-baseline-msi.sh <out-dir> [<release tag>] | --list}
want=${2:-}
sha=${GITHUB_SHA:?GITHUB_SHA is not set}
run_id=${GITHUB_RUN_ID:?GITHUB_RUN_ID is not set}
# Kept develop MSIs looked at, newest first, for one whose msi job passed.
max_candidates=${BASELINE_MAX_CANDIDATES:-10}
pattern='Puddle_*_x64_en-US.msi'

mkdir -p "$out"
[ -z "$(ls -A "$out")" ] || die "$out is not empty"

if [ -n "${BASELINE_RUN:-}" ]; then
    [[ $BASELINE_RUN =~ ^[0-9]+$ ]] || die "BASELINE_RUN is not a run id: $BASELINE_RUN"
    gh run download "$BASELINE_RUN" -R "$repo" --name puddle-msi --dir "$out" ||
        die "could not download the MSI of run $BASELINE_RUN (kept 14 days)"
    source="msi run $BASELINE_RUN (asked for)"
else
    if [ -n "$want" ]; then
        tag=$want
    else
        tag=$(gh release list -R "$repo" --exclude-drafts --limit 1 --json tagName --jq '.[0].tagName // ""') ||
            die "could not list the releases of $repo"
    fi
    if [ -n "$tag" ]; then
        gh release download "$tag" -R "$repo" --pattern "$pattern" --dir "$out" ||
            die "release $tag has no MSI ($pattern): an upgrade from it cannot be tested"
        source="release $tag"
    else
        candidates=$(gh api "repos/$repo/actions/artifacts?name=puddle-msi&per_page=100" --jq \
            ".artifacts[] | select(.expired | not) | select(.workflow_run.head_branch == \"develop\")
             | select(.workflow_run.head_sha != \"$sha\")
             | select(.workflow_run.id < $run_id) | \"\(.workflow_run.id) \(.workflow_run.head_sha)\"") ||
            die "could not list the kept MSIs of $repo"
        run="" from=""
        while read -r id head; do
            [ -n "$id" ] || continue
            # The job that built and installed the MSI, not the run: a run whose upgrade or usable
            # job is red still kept an MSI that installed, and judging the run made the baseline
            # unobtainable while the upgrade job itself was the red one.
            if [ "$(gh run view "$id" -R "$repo" --json jobs --jq '[.jobs[] | select(.name | startswith("msi (")) | .conclusion] | first // ""')" = success ]; then
                run=$id from=$head
                break
            fi
        done < <(printf '%s\n' "$candidates" | head -n "$max_candidates")
        [ -n "$run" ] || die "$repo has no release and no develop run of msi.yml with a passing msi job that kept an MSI (puddle-msi, kept 14 days) on another commit"
        gh run download "$run" -R "$repo" --name puddle-msi --dir "$out" || die "could not download the MSI of run $run"
        source="develop at ${from:0:9} (msi run $run; no release yet)"
    fi
fi

shopt -s nullglob
found=("$out"/Puddle_*_x64_en-US.msi)
[ "${#found[@]}" = 1 ] || die "expected one $pattern from $source in $out, found: $(ls -A "$out")"
echo "baseline: ${found[0]##*/} from $source" >&2
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
    echo "### upgrade baseline: ${found[0]##*/} from $source" >>"$GITHUB_STEP_SUMMARY"
fi
echo "${found[0]}"
