#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# A pull request whose net diff undoes a commit already on develop, without saying so, is a silent revert.
# It happens when a branch is rebuilt from an old snapshot or a conflict is resolved to the branch's side:
# #1241 (a VM-test change) put back the old text of four files that #1233 had fixed hours before, and the
# nightly installed-app walk went red for five days with nobody connecting it to #1241.
#
# What it does: for each commit of the last 14 days on the first-parent line of the base (at most 300), count
# the lines that commit added and the pull request now removes, plus the lines it removed and the pull request
# adds back (exact lines after trimming, 8 characters or more, per file). A commit counts as undone when 4 or
# more such lines are undone and they are at least half of the lines the commit changed. An undone commit fails
# the gate unless the pull request names it: its 8-character SHA or its `#<number>` in the title, the body or a
# commit message (a deliberate revert says `Reverts #1233`).
#
#   scripts/check-silent-revert.sh [BASE [HEAD]]   BASE defaults to scripts/diff-base.sh, HEAD to HEAD
#   scripts/check-silent-revert.sh --self-test     prove the gate on a throwaway repository
# The pull request's title and body come from $PR_TITLE and $PR_BODY (the workflow sets them from the event
# payload); a run without them sees only the commit messages. A rerun of a job reuses the old payload, so when
# $PR_NUMBER and $GH_TOKEN are set the script reads the live title and body with `gh pr view` instead and keeps
# the payload values only when that call fails: editing the body and rerunning the job clears the gate.
set -euo pipefail

DAYS=14
MAX=300
MIN_LINES=4

# Prints "<undone> <changed>" for one commit and one file diff pair: lines of the commit's diff (file 1) that
# the net diff (file 2) undoes, and the lines the commit changed.
undone() {
    awk -v min=8 '
        function norm(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
        function ok(s) { return length(s) >= min }
        FNR == 1 { f++ }
        /^(\+\+\+|---|@@|diff |index )/ { next }
        /^[+-]/ {
            t = norm(substr($0, 2)); if (!ok(t)) next
            if (f == 1) { if (substr($0, 1, 1) == "+") ca[t]++; else cr[t]++ }
            else        { if (substr($0, 1, 1) == "-") pd[t]++; else pa[t]++ }
        }
        END {
            for (t in ca) { total += ca[t]; u += (ca[t] < pd[t] ? ca[t] : pd[t]) }
            for (t in cr) { total += cr[t]; u += (cr[t] < pa[t] ? cr[t] : pa[t]) }
            print u + 0, total + 0
        }' "$1" "$2"
}

# Replaces PR_TITLE and PR_BODY with the live ones when the pull request can be read; otherwise keeps them.
refresh_pr_text() {
    local json
    [ -n "${PR_NUMBER:-}" ] && [ -n "${GH_TOKEN:-${GITHUB_TOKEN:-}}" ] || return 0
    command -v gh >/dev/null 2>&1 || return 0
    if json=$(GH_TOKEN="${GH_TOKEN:-${GITHUB_TOKEN:-}}" gh pr view "$PR_NUMBER" --json title,body 2>/dev/null) &&
        command -v jq >/dev/null 2>&1; then
        PR_TITLE=$(jq -r '.title // ""' <<<"$json")
        PR_BODY=$(jq -r '.body // ""' <<<"$json")
    else
        echo "silent-revert: could not read the live pull request; using the event payload" >&2
    fi
}

check() {
    local base=$1 head=$2 text c f files u t su st sha num subj tmp
    tmp=$(mktemp -d)
    # shellcheck disable=SC2064 # expand now: $tmp is local
    trap "rm -rf '$tmp'" RETURN
    refresh_pr_text
    text="${PR_TITLE:-}"$'\n'"${PR_BODY:-}"$'\n'"$(git log --format='%H %B' "$base..$head")"
    local fail=0
    git diff --name-only "$base" "$head" >"$tmp/changed"
    [ -s "$tmp/changed" ] || { echo "silent-revert: the change touches no file"; return 0; }
    while read -r c; do
        [ -n "$c" ] || continue
        [ "$(git rev-list --parents -n1 "$c" | wc -w)" -eq 2 ] || continue # a squash commit has one parent
        sha=${c:0:8}
        subj=$(git log -1 --format=%s "$c")
        num=$(sed -n 's/.*(#\([0-9][0-9]*\))$/\1/p' <<<"$subj")
        case $text in *"$sha"*) continue ;; esac
        if [ -n "$num" ] && grep -Eq "(^|[^0-9])#$num([^0-9]|$)" <<<"$text"; then continue; fi
        su=0 st=0
        files=$(git diff-tree --no-commit-id --name-only -r "$c" | grep -Fxf "$tmp/changed" || true)
        while read -r f; do
            [ -n "$f" ] || continue
            git diff -U0 "$c^" "$c" -- "$f" >"$tmp/c"
            git diff -U0 "$base" "$head" -- "$f" >"$tmp/p"
            [ -s "$tmp/p" ] || continue
            read -r u t < <(undone "$tmp/c" "$tmp/p")
            su=$((su + u)) st=$((st + t))
        done <<<"$files"
        if [ "$su" -ge "$MIN_LINES" ] && [ $((su * 2)) -ge "$st" ]; then
            echo "silent-revert: this change undoes $su of the $st lines that $sha ($subj) changed, and does not name it" >&2
            fail=1
        fi
    done < <(git log --first-parent --since="$DAYS days ago" --max-count="$MAX" --format=%H "$base")
    if [ "$fail" -ne 0 ]; then
        echo "silent-revert: if the revert is deliberate, say so in the PR body or a commit message (\`Reverts #<number>\` or the SHA);" >&2
        echo "silent-revert: if not, a stale branch or a conflict resolved to the old side put the old text back: re-apply the commit." >&2
        return 1
    fi
    echo "silent-revert: no merged commit of the last $DAYS days is undone without being named"
}

self_test() {
    local d out
    d=$(mktemp -d)
    # shellcheck disable=SC2064 # expand now: $d is local
    trap "rm -rf '$d'" RETURN
    (
        cd "$d"
        export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.com GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.com
        git init -q -b develop .
        printf 'alpha line one\nalpha line two\nalpha line three\nalpha line four\nalpha line five\n' >f.txt
        printf 'unrelated content that stays\n' >g.txt
        git add . && git commit -qm base
        printf 'alpha line one\nbeta fixed two\nbeta fixed three\nbeta fixed four\nbeta fixed five\n' >f.txt
        git commit -qam 'fix: rename the lines (#77)'
        git branch -q stale HEAD~1
        printf 'a new file with new lines\n' >h.txt
        git add . && git commit -qm 'feat: another change (#78)'
        git checkout -q -B pr-revert develop
        git show stale:f.txt >f.txt
        git commit -qam 'test: unrelated work'
        git checkout -q -B pr-named develop
        git show stale:f.txt >f.txt
        git commit -qam 'revert: undo the rename

Reverts #77'
        git checkout -q -B pr-innocent develop
        printf 'something else entirely, a long line\n' >>g.txt
        git commit -qam 'chore: add a line'
    )
    out=$(cd "$d" && check_in "$d" pr-revert 2>&1) && {
        echo "self-test: a silent revert passed: $out" >&2
        return 1
    }
    case $out in *"(fix: rename the lines (#77))"*) ;; *)
        echo "self-test: the silent revert was caught but not named: $out" >&2
        return 1
        ;;
    esac
    out=$(check_in "$d" pr-named 2>&1) || {
        echo "self-test: a revert that names #77 failed: $out" >&2
        return 1
    }
    out=$(check_in "$d" pr-innocent 2>&1) || {
        echo "self-test: an unrelated change failed: $out" >&2
        return 1
    }
    # The live body wins over a stale payload: a stub gh returns a body that names #77.
    mkdir "$d/bin"
    printf '#!/bin/sh\necho "{\\"title\\":\\"t\\",\\"body\\":\\"Reverts #77\\"}"\n' >"$d/bin/gh"
    chmod +x "$d/bin/gh"
    out=$(PATH="$d/bin:$PATH" PR_NUMBER=1 GH_TOKEN=x PR_BODY=stale check_in "$d" pr-revert 2>&1) || {
        echo "self-test: a live body that names #77 did not clear the gate: $out" >&2
        return 1
    }
    printf '#!/bin/sh\nexit 1\n' >"$d/bin/gh"
    out=$(PATH="$d/bin:$PATH" PR_NUMBER=1 GH_TOKEN=x PR_BODY='Reverts #77' check_in "$d" pr-revert 2>&1) || {
        echo "self-test: a failed live read did not fall back to the payload: $out" >&2
        return 1
    }
    echo "silent-revert self-test ok"
}

check_in() { (cd "$1" && check develop "$2"); }

main() {
    if [ "${1:-}" = --self-test ]; then
        self_test
        return
    fi
    local base=${1:-} head=${2:-HEAD}
    [ -n "$base" ] || base=$(scripts/diff-base.sh)
    if [ -z "$base" ]; then
        echo "silent-revert: no base commit; nothing to compare" >&2
        return 0
    fi
    check "$base" "$head"
}
main "$@"
