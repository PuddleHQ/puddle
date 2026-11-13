#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/fetch-baseline-msi.sh with a fake `gh` (no network): a release's MSI wins; a
# release without one fails; with no release, the newest green develop run on another commit that
# kept an MSI is taken, and runs of this commit, of other branches, expired ones and red ones are
# passed over; no candidate fails with the reason. The fake answers `--jq` with jq over a fixture,
# so the script's own filter is what selects. Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/fetch-baseline-msi.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/fetch-baseline-msi-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "fetch-baseline-msi-test: FAIL: $*" >&2; fail=1; }

mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# Fake gh: FAKE_TAG (the newest release, "" for none), FAKE_RELEASE_MSI (the asset it has, ""
# for none), FAKE_ARTIFACTS (the artifacts API's JSON), FAKE_CONCLUSIONS ("<run> <conclusion>"
# lines). Logs each call to FAKE_LOG.
echo "$*" >>"$FAKE_LOG"
jq_filter="" dir=""
args=("$@")
for ((i = 0; i < ${#args[@]}; i++)); do
    case ${args[i]} in
    --jq) jq_filter=${args[i + 1]} ;;
    --dir) dir=${args[i + 1]} ;;
    esac
done
case "$1 $2" in
"release list") printf '[%s]' "${FAKE_TAG:+{\"tagName\":\"$FAKE_TAG\"\}}" | jq -r "$jq_filter" ;;
"release download")
    [ -n "${FAKE_RELEASE_MSI:-}" ] || { echo "no assets match the file pattern" >&2; exit 1; }
    : >"$dir/$FAKE_RELEASE_MSI"
    ;;
"api repos/o/r/actions/artifacts?name=puddle-msi&per_page=100") jq -r "$jq_filter" "$FAKE_ARTIFACTS" ;;
"run view") awk -v id="$3" '$1 == id { print $2 }' "$FAKE_CONCLUSIONS" ;;
"run download") : >"$dir/Puddle_0.0.0_x64_en-US.msi" ;;
*) echo "fake gh: unexpected call: $*" >&2; exit 2 ;;
esac
GH
chmod +x "$tmp/bin/gh"

# artifact <run id> <branch> <sha> <expired>
artifact() { printf '{"expired":%s,"workflow_run":{"id":%s,"head_branch":"%s","head_sha":"%s"}}' "$4" "$1" "$2" "$3"; }
{
    printf '{"artifacts":['
    artifact 60 develop thiscommit false; printf ,
    artifact 59 feature/x aaaaaaaaaaaa false; printf ,
    artifact 58 develop bbbbbbbbbbbb true; printf ,
    artifact 57 develop cccccccccccc false; printf ,
    artifact 56 develop dddddddddddd false; printf ,
    artifact 55 develop eeeeeeeeeeee false
    printf ']}'
} >"$tmp/artifacts.json"
printf '60 success\n59 success\n58 success\n57 failure\n56 success\n55 success\n' >"$tmp/green"
printf '60 success\n57 failure\n' >"$tmp/red"

n=0
# run <case name> <VAR=value...> : runs the script into a fresh folder; sets rc, out, err, dir.
# The name is for the reader; failures name the case themselves.
run() {
    shift
    n=$((n + 1))
    dir="$tmp/out$n"
    : >"$tmp/log"
    : >"$tmp/summary"
    rc=0
    out=$(env PATH="$tmp/bin:$PATH" GITHUB_REPOSITORY=o/r GITHUB_SHA=thiscommit GITHUB_STEP_SUMMARY="$tmp/summary" \
        FAKE_LOG="$tmp/log" FAKE_ARTIFACTS="$tmp/artifacts.json" FAKE_CONCLUSIONS="$tmp/green" "$@" \
        "$script" "$dir" 2>"$tmp/err") || rc=$?
    err=$(cat "$tmp/err")
}

run "a release" FAKE_TAG=v0.2.0 FAKE_RELEASE_MSI=Puddle_0.2.0_x64_en-US.msi
[ "$rc" = 0 ] || bad "a release: exit $rc ($err)"
[ "$out" = "$dir/Puddle_0.2.0_x64_en-US.msi" ] || bad "a release: printed '$out'"
grep -q "from release v0.2.0" "$tmp/summary" || bad "a release: the summary does not name it: $(cat "$tmp/summary")"
grep -q "^api " "$tmp/log" && bad "a release: the kept develop MSIs were looked at too"

run "a release without an MSI" FAKE_TAG=v0.2.0 FAKE_RELEASE_MSI=
[ "$rc" = 1 ] || bad "a release without an MSI: exit $rc, want 1"
[[ $err == *"release v0.2.0 has no MSI"* ]] || bad "a release without an MSI: the reason is not named: $err"

run "no release" FAKE_TAG=
[ "$rc" = 0 ] || bad "no release: exit $rc ($err)"
[ "$out" = "$dir/Puddle_0.0.0_x64_en-US.msi" ] || bad "no release: printed '$out'"
grep -q "^run download 56 " "$tmp/log" || bad "no release: did not take run 56 (the newest green develop run on another commit): $(cat "$tmp/log")"
grep -qE "^run view (60|59|58) " "$tmp/log" && bad "no release: looked at a run of this commit, another branch or an expired MSI"
[[ $err == *"develop at ddddddddd (msi run 56; no release yet)"* ]] || bad "no release: the source is not named: $err"

run "no green run" FAKE_TAG= FAKE_CONCLUSIONS="$tmp/red"
[ "$rc" = 1 ] || bad "no green run: exit $rc, want 1"
[[ $err == *"no release and no green develop run"* ]] || bad "no green run: the reason is not named: $err"

run "few candidates" FAKE_TAG= BASELINE_MAX_CANDIDATES=1
[ "$rc" = 1 ] || bad "a red newest candidate within a limit of one: exit $rc, want 1"

mkdir -p "$tmp/out$((n + 1))"
: >"$tmp/out$((n + 1))/left-over"
run "a folder in use" FAKE_TAG=v0.2.0 FAKE_RELEASE_MSI=Puddle_0.2.0_x64_en-US.msi
[ "$rc" = 1 ] && [[ $err == *"is not empty"* ]] || bad "a folder in use: exit $rc ($err)"

if [ "$fail" = 0 ]; then echo "fetch-baseline-msi-test: ok"; else exit 1; fi
