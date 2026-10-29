#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/raise-coverage-baseline.sh: a bare origin, a checkout of its develop, the real ratchet and ci/scope.sh, and a
# fake `gh` over a table of green ci runs and their coverage summaries. Cases: a raise opens one pull request with one commit
# and arms auto-merge; the same raise again does nothing; a higher one updates the branch and opens no second pull request;
# nothing to raise closes a caught-up pull request; no measurement, or one only on a commit with code changes since, raises
# nothing; a documentation-only tip uses its parent's run; a refused pull request leaves the branch and exits 0; a dry run
# changes nothing; a measurement below the baseline fails; the UI baseline (ui/coverage-baseline.json, the `linux-ui-test-results`
# artifact) is raised alone, or in the same commit and pull request as the Rust one, never lowered, found on an older commit only
# when nothing the ui job runs for changed since, and left alone when unmeasured. Prints ok only when every case passes.
# A && B || bad is the intended shape of every check here.
# shellcheck disable=SC2015
set -uo pipefail
# A hook (pre-commit, pre-push) exports GIT_DIR and GIT_INDEX_FILE: without this every git call below would hit the real repository.
# shellcheck disable=SC2046
unset $(git rev-parse --local-env-vars)

here=$(cd "$(dirname "$0")" && pwd)
script="$here/raise-coverage-baseline.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/raise-coverage-baseline-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "raise-coverage-baseline-test: FAIL: $*" >&2; fail=1; }
G() { git -c user.email=t@t -c user.name=t "$@"; }
bl=scripts/coverage-baseline.json
ubl=ui/coverage-baseline.json
uj() { printf '{\n  "lines": %s,\n  "branches": %s,\n  "functions": %s,\n  "statements": %s\n}\n' "$1" "$2" "$3" "$4"; }
j() { printf '{\n  "lines": %s,\n  "regions": %s\n}\n' "$1" "$2"; }
usummary() { printf '{"total":{"lines":{"pct":%s},"branches":{"pct":%s},"functions":{"pct":%s},"statements":{"pct":%s}}}\n' "$1" "$2" "$3" "$4"; }
summary() { printf '{"data":[{"totals":{"lines":{"percent":%s},"regions":{"percent":%s}}}]}\n' "$1" "$2"; }

mkdir "$tmp/bin" "$tmp/art"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# fake gh over $FAKE_RUNS ("<sha> <run id>" lines), $FAKE_ART/<id> (the Rust summary of a run) and $FAKE_ART/ui-<id> (the UI one), $FAKE_PR (the open pull request's number)
echo "$*" >>"$FAKE_LOG"
case "$1 $2" in
"run list")
	sha=""
	while [ "$#" -gt 0 ]; do [ "$1" = --commit ] && sha=$2; shift; done
	awk -v s="$sha" '$1 == s { print $2 }' "$FAKE_RUNS"
	;;
"run download")
	id=$(printf '%s\n' "$@" | grep -E '^[0-9]+$' | head -n 1)
	dir=""
	name=""
	while [ "$#" -gt 0 ]; do
		[ "$1" = -D ] && dir=$2
		[ "$1" = -n ] && name=$2
		shift
	done
	case $name in linux-ui-test-results) src=$FAKE_ART/ui-$id ;; linux-test-results) src=$FAKE_ART/$id ;; *) exit 1 ;; esac
	[ -f "$src" ] || exit 1
	mkdir -p "$dir/cov" && cp "$src" "$dir/cov/coverage-summary.json"
	;;
"pr list") [ ! -f "$FAKE_PR" ] || cat "$FAKE_PR" ;;
"pr create")
	[ "${FAKE_CREATE_FAIL:-}" != 1 ] || exit 1
	echo 7 >"$FAKE_PR"
	echo https://github.com/o/r/pull/7
	;;
"pr merge" | "pr close") [ "$2" != close ] || rm -f "$FAKE_PR" ;;
*) exit 1 ;;
esac
GH
chmod +x "$tmp/bin/gh"

# A fresh origin and checkout: develop = base (baseline 93.5/92.5, floors 92/90, a docs file), no runs, no pull request.
fresh() {
	rm -rf "$tmp/origin.git" "$tmp/wt" "$tmp/log" "$tmp/runs" "$tmp/pr" "$tmp/art"
	mkdir "$tmp/art"
	: >"$tmp/log"
	: >"$tmp/runs"
	G init -q --bare "$tmp/origin.git"
	G clone -q "$tmp/origin.git" "$tmp/wt" 2>/dev/null
	(
		cd "$tmp/wt" && G checkout -q -b develop && mkdir scripts docs &&
			printf 'COV_LINES=92\nCOV_REGIONS=90\n' >scripts/check.sh && j 93.5 92.5 >"$bl" && mkdir ui && uj 99.7 91.2 99.6 99.6 >"$ubl" && echo a >docs/a.md &&
			G add . && G commit -qm base && G push -q origin develop
	)
}
tip() { G -C "$tmp/wt" rev-parse HEAD; }
# run <sha> <id> <lines> <regions>: the newest green ci run (gh lists newest first) on sha whose artifact holds that measurement
run() { { echo "$1 $2"; cat "$tmp/runs"; } >"$tmp/runs.new" && mv "$tmp/runs.new" "$tmp/runs"; summary "$3" "$4" >"$tmp/art/$2"; }
# land <rc-var>: runs the script in the checkout (back on develop first); output in $tmp/out
go() {
	G -C "$tmp/wt" checkout -q -f develop
	(
		cd "$tmp/wt" && PATH="$tmp/bin:$PATH" GH="$tmp/bin/gh" GITHUB_REPOSITORY=o/r FAKE_LOG="$tmp/log" FAKE_RUNS="$tmp/runs" \
			FAKE_ART="$tmp/art" FAKE_PR="$tmp/pr" GITHUB_STEP_SUMMARY="$tmp/summary" "$@" "$script"
	) >"$tmp/out" 2>&1
}
remote_bl() { G -C "$tmp/origin.git" show "ci/raise-coverage-baseline:$bl" 2>/dev/null | tr -d '\n '; }
remote_ubl() { G -C "$tmp/origin.git" show "ci/raise-coverage-baseline:$ubl" 2>/dev/null | tr -d '\n '; }
# urun <sha> <id> <lines> <branches> <functions> <statements>: a green ci run on sha whose UI artifact holds that measurement
urun() { { echo "$1 $2"; cat "$tmp/runs"; } >"$tmp/runs.new" && mv "$tmp/runs.new" "$tmp/runs"; usummary "$3" "$4" "$5" "$6" >"$tmp/art/ui-$2"; }
calls() { grep -c "$1" "$tmp/log"; }

# 1. a raise: one commit on the branch, one pull request, auto-merge armed.
fresh; run "$(tip)" 11 94.07 92.5
go env; rc=$?
[ $rc = 0 ] || bad "1: exit $rc: $(cat "$tmp/out")"
[ "$(remote_bl)" = '{"lines":94,"regions":92.5}' ] || bad "1: branch file is '$(remote_bl)'"
[ "$(G -C "$tmp/origin.git" rev-list --count develop..ci/raise-coverage-baseline 2>/dev/null)" = 1 ] || bad "1: not exactly one commit over develop"
[ "$(G -C "$tmp/origin.git" log -1 --format=%s ci/raise-coverage-baseline 2>/dev/null)" = "build: raise the coverage baseline" ] || bad "1: commit subject"
[ "$(calls '^pr create')" = 1 ] && [ "$(calls '^pr merge .*--auto --squash 7')" = 1 ] || bad "1: pull request not opened once with auto-merge: $(cat "$tmp/log")"
[ "$(G -C "$tmp/origin.git" show "develop:$bl" | tr -d '\n ')" = '{"lines":93.5,"regions":92.5}' ] || bad "1: develop's file changed"

# 2. the same measurement again: nothing happens.
before=$(G -C "$tmp/origin.git" rev-parse ci/raise-coverage-baseline)
: >"$tmp/log"
go env; rc=$?
[ $rc = 0 ] && [ "$(G -C "$tmp/origin.git" rev-parse ci/raise-coverage-baseline)" = "$before" ] && [ "$(calls '^pr create')" = 0 ] && grep -q 'already raises it' "$tmp/out" \
	|| bad "2: not a no-op (rc $rc): $(cat "$tmp/out")"

# 3. a higher measurement while the pull request is open: the branch is rebuilt, no second pull request.
run "$(tip)" 12 94.31 92.62
: >"$tmp/log"
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_bl)" = '{"lines":94.3,"regions":92.6}' ] && [ "$(G -C "$tmp/origin.git" rev-list --count develop..ci/raise-coverage-baseline)" = 1 ] \
	&& [ "$(calls '^pr create')" = 0 ] && [ "$(calls '^pr merge')" = 1 ] || bad "3: branch not updated in place (rc $rc, file '$(remote_bl)'): $(cat "$tmp/out")"

# 4. develop's baseline has caught up (nothing to raise) while a pull request is open: it is closed.
fresh; run "$(tip)" 11 94.07 92.5; go env
G -C "$tmp/wt" checkout -q develop && j 94 92.5 >"$tmp/wt/$bl" && G -C "$tmp/wt" commit -qam "raised" && G -C "$tmp/wt" push -q origin develop
run "$(tip)" 13 94.07 92.5
: >"$tmp/log"
go env; rc=$?
[ $rc = 0 ] && [ "$(calls '^pr close')" = 1 ] && [ ! -f "$tmp/pr" ] || bad "4: caught-up pull request not closed (rc $rc): $(cat "$tmp/out")"

# 5. nothing measured: nothing raised, exit 0, no branch.
fresh
go env; rc=$?
[ $rc = 0 ] && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null && grep -q 'nothing raised tonight' "$tmp/out" || bad "5: (rc $rc) $(cat "$tmp/out")"

# 6. the run is on the parent and the tip only changed documentation: the parent's measurement counts.
fresh; run "$(tip)" 11 94.07 92.5
( cd "$tmp/wt" && echo b >>docs/a.md && G commit -qam "docs: more" && G push -q origin develop )
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_bl)" = '{"lines":94,"regions":92.5}' ] || bad "6: documentation-only tip did not use the parent's run (rc $rc): $(cat "$tmp/out")"

# 7. the run is on the parent but the tip changed code: no measurement of this commit, nothing raised.
fresh; run "$(tip)" 11 94.07 92.5
( cd "$tmp/wt" && mkdir -p crates/x/src && echo 'fn a() {}' >crates/x/src/lib.rs && G add . && G commit -qm "code" && G push -q origin develop )
go env; rc=$?
[ $rc = 0 ] && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null && grep -q 'nothing raised tonight' "$tmp/out" || bad "7: (rc $rc) $(cat "$tmp/out")"

# 8. GitHub refuses the pull request: the branch stays, a warning names the commands, exit 0, no auto-merge call.
fresh; run "$(tip)" 11 94.07 92.5
go env FAKE_CREATE_FAIL=1; rc=$?
[ $rc = 0 ] && [ "$(remote_bl)" = '{"lines":94,"regions":92.5}' ] && grep -q '::warning::could not open the pull request' "$tmp/out" && grep -q 'gh pr create' "$tmp/summary" && [ "$(calls '^pr merge')" = 0 ] \
	|| bad "8: refused pull request not handled (rc $rc): $(cat "$tmp/out")"

# 9. a dry run reports and changes nothing.
fresh; run "$(tip)" 11 94.07 92.5
go env DRY_RUN=1; rc=$?
[ $rc = 0 ] && grep -q 'would raise it' "$tmp/out" && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null && [ "$(calls '^pr create')" = 0 ] \
	|| bad "9: dry run (rc $rc): $(cat "$tmp/out")"

# 10. a measurement below the baseline fails the job (the gate that went green on it would have failed too).
fresh; run "$(tip)" 11 93.0 92.5
go env; rc=$?
[ $rc != 0 ] && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null && grep -q 'below the baseline' "$tmp/out" || bad "10: a measurement below the baseline did not fail (rc $rc): $(cat "$tmp/out")"

# 11. only the UI measured: its baseline is raised (rounded down per field), the Rust one stays, one commit, one pull request.
fresh; urun "$(tip)" 21 99.84 91.25 99.6 99.6
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_ubl)" = '{"lines":99.8,"branches":91.2,"functions":99.6,"statements":99.6}' ] && [ "$(remote_bl)" = '{"lines":93.5,"regions":92.5}' ] \
	&& [ "$(G -C "$tmp/origin.git" rev-list --count develop..ci/raise-coverage-baseline)" = 1 ] && [ "$(calls '^pr create')" = 1 ] && grep -q 'no Rust measurement' "$tmp/out" \
	|| bad "11: UI-only raise (rc $rc, ui '$(remote_ubl)', rust '$(remote_bl)'): $(cat "$tmp/out")"

# 12. both measured and both up: the same single commit and pull request carries both files; the same night again is a no-op.
fresh; run "$(tip)" 11 94.07 92.5; urun "$(tip)" 21 99.84 91.25 99.9 99.6
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_bl)" = '{"lines":94,"regions":92.5}' ] && [ "$(remote_ubl)" = '{"lines":99.8,"branches":91.2,"functions":99.9,"statements":99.6}' ] \
	&& [ "$(G -C "$tmp/origin.git" rev-list --count develop..ci/raise-coverage-baseline)" = 1 ] && [ "$(calls '^pr create')" = 1 ] && [ "$(calls '^pr merge')" = 1 ] \
	|| bad "12: both files not raised in one commit (rc $rc): $(cat "$tmp/out")"
before=$(G -C "$tmp/origin.git" rev-parse ci/raise-coverage-baseline)
: >"$tmp/log"
go env; rc=$?
[ $rc = 0 ] && [ "$(G -C "$tmp/origin.git" rev-parse ci/raise-coverage-baseline)" = "$before" ] && [ "$(calls '^pr create')" = 0 ] && grep -q 'already raises it' "$tmp/out" \
	|| bad "12: second night not a no-op (rc $rc): $(cat "$tmp/out")"
# a higher UI number alone (Rust unchanged) updates the open pull request in place
urun "$(tip)" 22 99.95 91.25 99.9 99.6
: >"$tmp/log"
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_ubl)" = '{"lines":99.9,"branches":91.2,"functions":99.9,"statements":99.6}' ] && [ "$(calls '^pr create')" = 0 ] \
	&& [ "$(G -C "$tmp/origin.git" rev-list --count develop..ci/raise-coverage-baseline)" = 1 ] || bad "12: UI-only update of the open pull request (rc $rc, ui '$(remote_ubl)'): $(cat "$tmp/out")"

# 13. the Rust baseline is already caught up but the UI one is not: the pull request opens for the UI file alone.
fresh; run "$(tip)" 11 93.5 92.5; urun "$(tip)" 21 99.84 91.25 99.6 99.6
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_bl)" = '{"lines":93.5,"regions":92.5}' ] && [ "$(remote_ubl)" = '{"lines":99.8,"branches":91.2,"functions":99.6,"statements":99.6}' ] \
	|| bad "13: UI-only difference not raised (rc $rc): $(cat "$tmp/out")"

# 14. a UI measurement below its baseline fails the job and raises nothing, not even the Rust file (the ui gate went green on it only if the baseline was lower).
fresh; run "$(tip)" 11 94.07 92.5; urun "$(tip)" 21 99.0 91.2 99.6 99.6
go env; rc=$?
[ $rc != 0 ] && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null && grep -q 'ui coverage ratchet: lines coverage .* below the baseline' "$tmp/out" \
	|| bad "14: a UI measurement below the baseline did not fail (rc $rc): $(cat "$tmp/out")"

# 15. neither ratchet ever writes a lower number: a measurement equal to the baselines changes no file and opens nothing.
fresh; run "$(tip)" 11 93.5 92.5; urun "$(tip)" 21 99.7 91.2 99.6 99.6
go env; rc=$?
[ $rc = 0 ] && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null && [ "$(calls '^pr create')" = 0 ] && grep -q 'nothing to raise' "$tmp/out" \
	|| bad "15: equal measurements raised something (rc $rc): $(cat "$tmp/out")"

# 16. the UI run is on the parent and the tip changed only a crate the ui job does not build: the parent's UI measurement counts.
fresh; urun "$(tip)" 21 99.84 91.25 99.6 99.6
( cd "$tmp/wt" && mkdir -p crates/xtask/src && echo 'fn a() {}' >crates/xtask/src/lib.rs && G add . && G commit -qm "xtask" && G push -q origin develop )
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_ubl)" = '{"lines":99.8,"branches":91.2,"functions":99.6,"statements":99.6}' ] || bad "16: ui-untouched tip did not use the parent's UI run (rc $rc): $(cat "$tmp/out")"

# 17. the UI run is on the parent but the tip changed ui/: that commit has no UI measurement, nothing is raised.
fresh; urun "$(tip)" 21 99.84 91.25 99.6 99.6
( cd "$tmp/wt" && mkdir -p ui/src && echo 'export {};' >ui/src/a.ts && G add . && G commit -qm "ui change" && G push -q origin develop )
go env; rc=$?
[ $rc = 0 ] && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null && grep -q 'nothing raised tonight' "$tmp/out" || bad "17: (rc $rc) $(cat "$tmp/out")"

# 18. a dry run with both files up reports both and changes nothing.
fresh; run "$(tip)" 11 94.07 92.5; urun "$(tip)" 21 99.84 91.25 99.9 99.6
go env DRY_RUN=1; rc=$?
[ $rc = 0 ] && grep -q 'would raise it (rust: .*; ui: ' "$tmp/out" && ! G -C "$tmp/origin.git" rev-parse -q --verify ci/raise-coverage-baseline >/dev/null || bad "18: dry run (rc $rc): $(cat "$tmp/out")"

# 19. a night that measures only one of the two keeps the other's pending raise: the open pull request is neither rebuilt without it nor closed.
fresh; T=$(tip); run "$T" 11 94.07 92.5; urun "$T" 21 99.84 91.25 99.9 99.6
go env
before=$(G -C "$tmp/origin.git" rev-parse ci/raise-coverage-baseline)
: >"$tmp/runs"; : >"$tmp/log"
urun "$T" 23 99.84 91.25 99.9 99.6
go env; rc=$?
[ $rc = 0 ] && [ "$(G -C "$tmp/origin.git" rev-parse ci/raise-coverage-baseline)" = "$before" ] && [ "$(calls '^pr create')" = 0 ] && [ "$(calls '^pr close')" = 0 ] && [ -f "$tmp/pr" ] \
	&& grep -q 'already raises it' "$tmp/out" || bad "19: UI-only night changed the pull request holding both raises (rc $rc, rust '$(remote_bl)'): $(cat "$tmp/out")"
# the Rust measurement alone, with the UI baseline caught up on develop: the pull request keeps only what is still above develop
: >"$tmp/runs"; : >"$tmp/log"
G -C "$tmp/wt" checkout -q -f develop && uj 99.8 91.2 99.9 99.6 >"$tmp/wt/$ubl" && G -C "$tmp/wt" commit -qam "ui raised" && G -C "$tmp/wt" push -q origin develop
run "$(G -C "$tmp/wt" rev-parse develop)" 24 94.07 92.5
go env; rc=$?
[ $rc = 0 ] && [ "$(remote_bl)" = '{"lines":94,"regions":92.5}' ] && [ "$(remote_ubl)" = '{"lines":99.8,"branches":91.2,"functions":99.9,"statements":99.6}' ] && [ "$(calls '^pr close')" = 0 ] \
	|| bad "19: Rust-only night after the UI baseline caught up (rc $rc, ui '$(remote_ubl)'): $(cat "$tmp/out")"

if [ $fail = 0 ]; then
	echo "raise-coverage-baseline-test: ok"
fi
exit $fail
