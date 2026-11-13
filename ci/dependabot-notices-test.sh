#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/dependabot-notices.sh with a fake `gh` and a local bare repository as the remote. The main case is
# the stale-header one from a real npm group PR: ci/fixtures/dependabot-notices/stale.txt is the notices file at the
# Dependabot head and fresh.txt what `npm run licences` made of it (a 5-line diff of version headers). Checks that
# one bot commit with only that file is fast-forwarded onto the branch, the four required workflows are dispatched,
# auto-merge is re-armed only when it was on, and every refusal pushes nothing. Prints ok only when all pass.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
# Run from a git hook these variables point at the calling repository, which the throwaway repos below must never touch.
# shellcheck disable=SC1091 # the helper sits in scripts/
. "$here/../scripts/git-clean-env.sh"
# Also author and committer variables: they would override the bot identity the assertions check.
while IFS= read -r v; do unset "$v"; done < <(compgen -e | grep '^GIT_' || true)
script="$here/dependabot-notices.sh"
fx="$here/fixtures/dependabot-notices"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/dependabot-notices-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "dependabot-notices-test: FAIL: $*" >&2; fail=1; }
git_() { git -c user.name=dependabot[bot] -c user.email=bot@example.com -c commit.gpgsign=false "$@"; }

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
echo "$*" >>"$FAKE_LOG"
case "$1" in
api) cat "$FAKE_PRS" ;;
workflow) [ "${FAKE_DISPATCH_FAIL:-}" = 1 ] && exit 1 ;;
esac
exit 0
GH
chmod +x "$tmp/bin/gh"

B=dependabot/npm_and_yarn/ui/develop/npm-abc123
n=0
remote=
# A new remote with develop and the Dependabot branch holding the stale file; sets remote and tip.
mkremote() {
    n=$((n + 1))
    remote="$tmp/remote$n.git"
    git init -q --bare -b develop "$remote"
    git init -q -b develop "$tmp/seed$n"
    (cd "$tmp/seed$n" && mkdir ui && cp "$fx/stale.txt" ui/THIRD-PARTY-NOTICES.txt && echo '{}' >ui/package-lock.json &&
        git_ add . && git_ commit -qm base && git_ checkout -qb "$B" &&
        echo '{"bumped":1}' >ui/package-lock.json && git_ commit -qam 'build(deps): bump the npm group' &&
        git remote add origin "file://$remote" && git push -q origin develop "$B")
    tip=$(git -C "$remote" rev-parse "$B")
}
# prs <head sha> [user] [auto_merge json] [repo] -> a one-PR list
prs() {
    jq -n --arg sha "$1" --arg user "${2:-dependabot[bot]}" --argjson am "${3:-null}" --arg repo "${4:-o/r}" \
        '[{number: 7, draft: false, user: {login: $user}, head: {sha: $sha, repo: {full_name: $repo}}, auto_merge: $am}]'
}
# run <branch> <head sha> <file> <prs file> -> sets out, rc
run() {
    : >"$tmp/log"
    out=$(FAKE_PRS=$4 FAKE_LOG=$tmp/log PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r NOTICES_REMOTE="file://$remote" "$script" "$1" "$2" "$3" 2>&1)
    rc=$?
}
unchanged() { # label tip: the remote branch is where it was and nothing was dispatched or armed
    [ "$(git -C "$remote" rev-parse "$B")" = "$2" ] || bad "$1: branch moved but nothing should be pushed"
    ! grep -q '^workflow\|^pr merge' "$tmp/log" || bad "$1: dispatched or armed: $(cat "$tmp/log")"
}
says() { # label fragment: exit 0 and the output names the reason
    [ "$rc" = 0 ] || bad "$1: exit $rc: $out"
    case $out in *"$2"*) ;; *) bad "$1: output lacks '$2': $out" ;; esac
}

# 1. the stale-header case: one bot commit, only the notices file, a 5-line diff, four dispatches, no arming
mkremote
prs "$tip" >"$tmp/prs"
run "$B" "$tip" "$fx/fresh.txt" "$tmp/prs"
[ "$rc" = 0 ] || bad "stale: exit $rc: $out"
new=$(git -C "$remote" rev-parse "$B")
[ "$new" != "$tip" ] || bad "stale: nothing pushed: $out"
[ "$(git -C "$remote" rev-parse "$B~1")" = "$tip" ] || bad "stale: not one fast-forward commit on the old tip"
[ "$(git -C "$remote" diff --name-only "$tip" "$new")" = ui/THIRD-PARTY-NOTICES.txt ] || bad "stale: commit touches more than the notices"
git -C "$remote" show "$new:ui/THIRD-PARTY-NOTICES.txt" | cmp -s - "$fx/fresh.txt" || bad "stale: file is not the regenerated one"
[ "$(git -C "$remote" diff --numstat "$tip" "$new")" = "$(printf '5\t5\tui/THIRD-PARTY-NOTICES.txt')" ] || bad "stale: diff is not the 5-line header diff: $(git -C "$remote" diff --numstat "$tip" "$new")"
[ "$(git -C "$remote" log -1 --format=%an "$new")" = 'github-actions[bot]' ] || bad "stale: author is not the bot"
for wf in ci windows vm-linux vm-windows; do
    grep -qx "workflow run $wf.yml -R o/r --ref $B" "$tmp/log" || bad "stale: $wf.yml not dispatched: $(cat "$tmp/log")"
done
! grep -q '^pr merge' "$tmp/log" || bad "stale: armed a PR that had auto-merge off"

# 2. auto-merge already on: re-armed for the new head
mkremote
prs "$tip" 'dependabot[bot]' '{"merge_method":"squash"}' >"$tmp/prs"
run "$B" "$tip" "$fx/fresh.txt" "$tmp/prs"
new=$(git -C "$remote" rev-parse "$B")
grep -qx "pr merge 7 -R o/r --auto --squash --match-head-commit $new" "$tmp/log" || bad "rearm: not re-armed at $new: $(cat "$tmp/log")"

# 3. refusals, all on one remote whose tip never moves
mkremote
prs "$tip" >"$tmp/prs"
run "$B" "$tip" "$fx/stale.txt" "$tmp/prs"
says current "already current"
unchanged current "$tip"
run "dependabot/npm_and_yarn/crates/puddle-api/openapi/develop/x-1" "$tip" "$fx/fresh.txt" "$tmp/prs"
says branch "not a ui npm branch"
unchanged branch "$tip"
prs "$tip" octocat >"$tmp/prs2"
run "$B" "$tip" "$fx/fresh.txt" "$tmp/prs2"
says human "not an own-repo Dependabot PR"
unchanged human "$tip"
prs "$tip" 'dependabot[bot]' null fork/r >"$tmp/prs2"
run "$B" "$tip" "$fx/fresh.txt" "$tmp/prs2"
says fork "not an own-repo Dependabot PR"
unchanged fork "$tip"
echo '[]' >"$tmp/prs2"
run "$B" "$tip" "$fx/fresh.txt" "$tmp/prs2"
says "no pr" "expected one open PR"
unchanged "no pr" "$tip"
prs 0000000000000000000000000000000000000000 >"$tmp/prs2"
run "$B" "$tip" "$fx/fresh.txt" "$tmp/prs2"
says moved "branch moved"
unchanged moved "$tip"
run "$B" 1111111111111111111111111111111111111111 "$fx/fresh.txt" "$tmp/prs"
says "stale build" "branch moved"
unchanged "stale build" "$tip"
printf 'not the notices\n' >"$tmp/junk"
run "$B" "$tip" "$tmp/junk" "$tmp/prs"
says heading "lacks the notices heading"
unchanged heading "$tip"
ln -s "$fx/fresh.txt" "$tmp/link"
run "$B" "$tip" "$tmp/link" "$tmp/prs"
says symlink "not a regular file"
unchanged symlink "$tip"
{ cat "$fx/fresh.txt"; head -c 2200000 /dev/zero | tr '\0' x; } >"$tmp/big"
run "$B" "$tip" "$tmp/big" "$tmp/prs"
says big "over 2 MiB"
unchanged big "$tip"

# 4. a failing dispatch fails the job, with the command to rerun by hand
mkremote
prs "$tip" >"$tmp/prs"
: >"$tmp/log"
out=$(FAKE_DISPATCH_FAIL=1 FAKE_PRS=$tmp/prs FAKE_LOG=$tmp/log PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r NOTICES_REMOTE="file://$remote" "$script" "$B" "$tip" "$fx/fresh.txt" 2>&1)
rc=$?
[ "$rc" != 0 ] || bad "dispatch fail: exit 0"
case $out in *"gh workflow run ci.yml"*) ;; *) bad "dispatch fail: $out" ;; esac

# 5. Dependabot pushes between the PR lookup and the clone: the tip check refuses, nothing is forced
mkremote
prs "$tip" >"$tmp/prs"
(cd "$tmp/seed$n" && echo '{"bumped":2}' >ui/package-lock.json && git_ commit -qam 'newer dependabot push' && git push -q origin "$B")
newer=$(git -C "$remote" rev-parse "$B")
run "$B" "$tip" "$fx/fresh.txt" "$tmp/prs"
says race "branch tip is not"
unchanged race "$newer"

[ "$fail" = 0 ] && echo "dependabot-notices-test: ok"
exit "$fail"
