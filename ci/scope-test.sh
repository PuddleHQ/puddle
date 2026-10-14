#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/scope.sh: replays the merged pull requests in ci/scope-prs.txt, checks that every
# always-run path forces a full run, plants paths the rules do not know to show the fail-open, drives
# the whole script with a fake `gh` (a failed, stale, empty or huge file list, other events), runs the
# result job's verdict on every combination that matters, and checks the four workflows are wired to
# match (python3 with PyYAML; skipped without it). SCOPE_TEST_METADATA=1 also checks the crate sets
# against `cargo metadata` (needs cargo and python3). Prints ok only when every case passes.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
scope="$here/scope.sh"
fixture="$here/scope-prs.txt"
fail=0
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

bad() { echo "FAIL: $*" >&2; fail=1; }

# classify <paths...> -> "class ci_rust win_rust ui vm"
classify() {
    printf '%s\n' "$@" | "$scope" classify | sort | awk -F= '{v[$1]=$2} END {print v["class"], v["ci_rust"], v["win_rust"], v["ui"], v["vm"]}'
}
want() { # want <label> <expected> <paths...>
    local label=$1 exp=$2 got
    shift 2
    got=$(classify "$@")
    [ "$got" = "$exp" ] || bad "$label: got '$got', want '$exp'"
}
FULL="full true true true true"

# 1. Replay of the merged pull requests: class and outputs as recorded, and an independent check
# that any pull request with an always-run path got a full run.
always_re='^(Cargo\.lock|Cargo\.toml|rust-toolchain\.toml|\.cargo/|\.config/|\.githooks/|\.github/workflows/|ci/|scripts/|crates/puddle-vm-tests/|crates/puddle-api/openapi/|ui/package-lock\.json|ui/src/lib/api/schema\.d\.ts|.*/Cargo\.toml$)'
data_re='^scripts/(diff-coverage-exclusions\.txt|coverage-baseline\.json|standalone-allow\.txt)$'
replayed=0
declare -A seen_class=()
pr="" exp="" paths=()
flush() {
    [ -n "$pr" ] || return 0
    local got always=0 p
    got=$(classify "${paths[@]}")
    [ "$got" = "$exp" ] || bad "pr $pr: got '$got', want '$exp'"
    for p in "${paths[@]}"; do
        if grep -Eq "$always_re" <<<"$p" && ! grep -Eq "$data_re" <<<"$p"; then always=1; fi
    done
    if [ "$always" = 1 ] && [ "$got" != "$FULL" ]; then bad "pr $pr has an always-run path but got '$got'"; fi
    seen_class[${got%% *}]=1
    replayed=$((replayed + 1))
}
while IFS= read -r line; do
    case $line in
    '#'* | '') ;;
    @*)
        flush
        pr=${line%% *}
        pr=${pr#@}
        exp=${line#* }
        paths=()
        ;;
    *) paths+=("$line") ;;
    esac
done <"$fixture"
flush
[ "$replayed" -ge 58 ] || bad "fixture replayed only $replayed pull requests (want at least 58)"
for c in full docs ui rust mixed; do
    [ -n "${seen_class[$c]:-}" ] || bad "the fixture has no pull request of class $c"
done

# 2. Every always-run path alone, and added to a docs-only and to a UI-only change, runs everything.
for p in Cargo.lock Cargo.toml rust-toolchain.toml .cargo/config.toml .config/nextest.toml .githooks/pre-push \
    .github/workflows/ci.yml ci/scope.sh ci/msb-runtime.sha256 scripts/check.sh scripts/gate-sched.sh \
    crates/puddle-vm-tests/src/lib.rs crates/puddle-vm-tests/tests/x.rs crates/puddle-api/openapi/openapi.json \
    crates/puddle-agent/Cargo.toml ui/package-lock.json ui/src/lib/api/schema.d.ts ui/THIRD-PARTY-NOTICES.txt \
    crates/puddle-store/tests/data/repo-urls.json .github/actions/x/action.yml ui/scripts/diff-coverage.ts ui/scripts/coverage-ratchet.ts deny.toml about.toml clippy.toml rustfmt.toml _typos.toml \
    .gitleaks.toml .gitattributes; do
    want "always-run $p alone" "$FULL" "$p"
    want "always-run $p with docs" "$FULL" docs/STANDARDS.md "$p"
    want "always-run $p with ui" "$FULL" ui/src/lib/x.ts "$p"
done

# 3. Planted misses: a path no rule names, an empty list and a huge one must fall open.
want "unknown top-level file" "$FULL" docs/STANDARDS.md Makefile
want "unknown directory" "$FULL" ui/src/lib/x.ts tools/new/run.sh
want "unknown crate-less crates path" "$FULL" crates/README
want "docs plus one VM-test crate file" "$FULL" docs/STANDARDS.md README.md crates/puddle-vm-tests/src/lib.rs
want "docs plus one new script" "$FULL" docs/adr/0001.md scripts/new-gate.sh
want "empty list" "$FULL"
mapfile -t many < <(seq 1 300 | sed 's|^|docs/page-|')
want "300 docs files" "$FULL" "${many[@]}"
mapfile -t many < <(seq 1 299 | sed 's|^|docs/page-|')
want "299 docs files" "docs false false false false" "${many[@]}"

# 4. The classes, by example.
want "docs only" "docs false false false false" docs/STANDARDS.md README.md .github/dependabot.yml
want "ui only" "ui false false true false" ui/src/lib/a.ts ui/e2e/b.spec.ts docs/x.md
want "ui and gate data" "mixed true false true false" ui/src/a.ts scripts/diff-coverage-exclusions.txt
want "gate data only" "rust true false true false" scripts/coverage-baseline.json
want "ui fixtures" "mixed true true true false" ui/e2e/fixtures/workspaces.json
want "vm crate" "rust true true true true" crates/puddle-compute-msb/src/lib.rs
want "host-side crate outside the UI closure" "rust true true false true" crates/puddle-lifecycle/src/lib.rs
want "crate outside the VM set, in the UI closure" "rust true true true false" crates/puddle-e2e/src/lib.rs
want "crate in neither" "rust true true false false" crates/xtask/src/main.rs
want "crate with a README only" "rust true true false true" crates/puddle-host/README.md
want "rename out of an always-run path" "$FULL" ui/src/a.ts ci/old.sh

# 5. The whole script with a fake `gh`.
mkdir "$tmp/bin"
cat >"$tmp/bin/gh" <<'GH'
#!/usr/bin/env bash
# fake gh: `gh api repos/R/pulls/N` -> head sha; `gh api --paginate repos/R/pulls/N/files...` -> $FAKE_FILES
case "$*" in
*/files*) [ "${FAKE_MODE:-}" = error ] && exit 1; cat "$FAKE_FILES" ;;
*) [ "${FAKE_MODE:-}" = headerror ] && exit 1; echo "${FAKE_HEAD:-abc123}" ;;
esac
GH
chmod +x "$tmp/bin/gh"
printf 'ui/src/a.ts\nui/src/b.ts\n' >"$tmp/ui.txt"
: >"$tmp/empty.txt"

# run_main <event> <base> <FAKE_MODE> <files> [PR_HEAD_SHA] -> key=value lines
run_main() {
    local event=$1 base=$2 mode=$3 files=$4 head=${5:-abc123}
    env PATH="$tmp/bin:$PATH" GITHUB_EVENT_NAME="$event" PR_BASE_REF="$base" PR_NUMBER=7 GITHUB_REPOSITORY=o/r \
        PR_HEAD_SHA="$head" FAKE_MODE="$mode" FAKE_FILES="$files" FAKE_HEAD=abc123 GITHUB_OUTPUT="$tmp/out" \
        "$scope" 2>/dev/null
    local rc=$?
    echo "rc=$rc"
}
main_case() { # main_case <label> <want class> <args of run_main>
    local label=$1 exp=$2 out
    shift 2
    : >"$tmp/out"
    out=$(run_main "$@")
    grep -q "^class=$exp$" <<<"$out" || bad "main: $label: want class $exp, got: $(tr '\n' ' ' <<<"$out")"
    grep -q '^rc=0$' <<<"$out" || bad "main: $label: exit status not 0"
    grep -q "^class=$exp$" "$tmp/out" || bad "main: $label: GITHUB_OUTPUT lacks class=$exp"
}
main_case "ui-only pull request to develop" ui pull_request develop ok "$tmp/ui.txt"
main_case "push" full push develop ok "$tmp/ui.txt"
main_case "schedule" full schedule develop ok "$tmp/ui.txt"
main_case "workflow_dispatch" full workflow_dispatch develop ok "$tmp/ui.txt"
main_case "pull request to main" full pull_request main ok "$tmp/ui.txt"
main_case "file list fails" full pull_request develop error "$tmp/ui.txt"
main_case "head lookup fails" full pull_request develop headerror "$tmp/ui.txt"
main_case "stale head" full pull_request develop ok "$tmp/ui.txt" deadbeef
main_case "empty list" full pull_request develop ok "$tmp/empty.txt"
printf 'ui/src/a.ts\nci/old.sh\n' >"$tmp/rename.txt"
main_case "rename from an always-run path" full pull_request develop ok "$tmp/rename.txt"
out=$(env -u GITHUB_EVENT_NAME PATH="$tmp/bin:$PATH" "$scope" 2>/dev/null | grep -c '^class=full$')
[ "$out" = 1 ] || bad "main: no event at all must run everything"
# A broken classifier (here: no gh and no PATH tools worth the name) still prints a full run.
out=$(env PATH="$tmp/nothing" GITHUB_EVENT_NAME=pull_request PR_BASE_REF=develop PR_NUMBER=7 GITHUB_REPOSITORY=o/r \
    "$(command -v bash)" "$scope" 2>/dev/null | grep -c '^class=full$')
[ "$out" = 1 ] || bad "main: missing gh must run everything"

# 6. The result job's verdict. needs <scope outputs> <job=result>... -> NEEDS json
needs() {
    local sc=$1 j
    shift
    local o="{\"scope\":{\"result\":\"success\",\"outputs\":$sc}"
    for j in "$@"; do o="$o,\"${j%%=*}\":{\"result\":\"${j#*=}\"}"; done
    echo "$o}"
}
verdict() { # verdict <event> <want 0|1> <label> <scope json> <specs...> -- <job=result>...
    local event=$1 exp=$2 label=$3 sc=$4 specs=() rc=0
    shift 4
    while [ "$1" != -- ]; do specs+=("$1"); shift; done
    shift
    NEEDS=$(needs "$sc" "$@") GITHUB_EVENT_NAME=$event "$scope" verdict "${specs[@]}" >/dev/null 2>&1 || rc=$?
    [ "$rc" = "$exp" ] || bad "verdict: $label: exit $rc, want $exp"
}
VM='{"vm":"true"}'
CI_FULL='{"ci_rust":"true","ui":"true"}'
VMF='{"vm":"false"}'
verdict pull_request 0 "vm ran and passed" "$VM" build=vm tests=vm -- build=success tests=success
verdict pull_request 0 "vm skipped by scope" "$VMF" build=vm tests=vm -- build=skipped tests=skipped
verdict pull_request 1 "vm skipped but scope said run" "$VM" build=vm tests=vm -- build=skipped tests=skipped
verdict pull_request 1 "vm skipped, scope outputs lost" '{}' build=vm tests=vm -- build=skipped tests=skipped
verdict pull_request 1 "tests skipped after a failed build" "$VM" build=vm tests=vm -- build=failure tests=skipped
verdict pull_request 1 "tests skipped, build passed (infrastructure)" "$VM" build=vm tests=vm -- build=success tests=skipped
verdict pull_request 1 "a failed test job" "$VMF" build=vm tests=vm -- build=skipped tests=failure
verdict pull_request 1 "a cancelled build" "$VM" build=vm tests=vm -- build=cancelled tests=skipped
verdict pull_request 1 "a job missing from needs" "$VM" build=vm tests=vm -- build=success
verdict schedule 0 "nightly skips tests on a green commit" "$VM" --nightly-skips build=vm tests=vm -- build=success tests=skipped
verdict schedule 0 "nightly skips everything" "$VM" --nightly-skips build=vm tests=vm -- build=skipped tests=skipped
verdict schedule 1 "nightly failure" "$VM" --nightly-skips build=vm tests=vm -- build=success tests=failure
verdict push 1 "a skipped job off a pull request without the flag" "$CI_FULL" rust=ci_rust -- rust=skipped
verdict push 0 "ci push, all passed" "$CI_FULL" rust=ci_rust ui=ui -- rust=success ui=success
CI_UI='{"ci_rust":"false","ui":"true"}'
CI_DOCS='{"ci_rust":"false","ui":"false"}'
ci_specs=(rust=ci_rust 'light=!ci_rust' ui=ui)
verdict pull_request 0 "ci full run" "$CI_FULL" "${ci_specs[@]}" -- rust=success light=skipped ui=success
verdict pull_request 0 "ci ui-only: light and ui" "$CI_UI" "${ci_specs[@]}" -- rust=skipped light=success ui=success
verdict pull_request 0 "ci docs-only: light only" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped light=success ui=skipped
verdict pull_request 1 "ci docs-only: light failed" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped light=failure ui=skipped
verdict pull_request 1 "ci docs-only: light skipped too" "$CI_DOCS" "${ci_specs[@]}" -- rust=skipped light=skipped ui=skipped
verdict pull_request 1 "ci full: rust skipped, light ran" "$CI_FULL" "${ci_specs[@]}" -- rust=skipped light=success ui=success
verdict pull_request 1 "ci full: rust failed" "$CI_FULL" "${ci_specs[@]}" -- rust=failure light=skipped ui=success
verdict pull_request 1 "ci ui job skipped though needed" "$CI_UI" "${ci_specs[@]}" -- rust=skipped light=success ui=skipped
verdict pull_request 0 "ci scope failed: everything ran" '{}' "${ci_specs[@]}" -- rust=success light=skipped ui=success
verdict pull_request 1 "ci scope failed: rust skipped" '{}' "${ci_specs[@]}" -- rust=skipped light=success ui=success
# Windows: the release build is not scoped (its own event rule skips it on a pull request to develop).
WIN_UI='{"win_rust":"false","ui":"true"}'
verdict pull_request 0 "windows ui-only, release skipped" "$WIN_UI" clippy=win_rust tests=win_rust ui=ui -- \
    clippy=skipped tests=skipped ui=success release=skipped
verdict pull_request 1 "windows ui-only, ui failed" "$WIN_UI" clippy=win_rust tests=win_rust ui=ui -- \
    clippy=skipped tests=skipped ui=failure release=skipped
verdict pull_request 1 "windows release failed" "$WIN_UI" clippy=win_rust tests=win_rust ui=ui -- \
    clippy=skipped tests=skipped ui=success release=failure
[ "$(NEEDS='' GITHUB_EVENT_NAME=pull_request "$scope" verdict x=y >/dev/null 2>&1; echo $?)" = 1 ] || bad "verdict: empty NEEDS must fail"

# 7. The workflows are wired to match their result job's verdict (needs PyYAML).
if python3 -c 'import yaml' 2>/dev/null; then
    python3 -I - "$here/../.github/workflows" <<'PY' || fail=1
import re, sys, yaml
d = sys.argv[1]
results = {  # workflow: (result job id, required name)
    "ci": ("linux", "linux (all gates)"),
    "windows": ("windows", "windows-msvc (build, clippy, tests)"),
    "vm-linux": ("vm-linux", "vm-linux (KVM, msb microVMs)"),
    "vm-windows": ("vm-windows", "vm-windows (WHP, msb microVMs)"),
}
bad = []
for wf, (rid, rname) in results.items():
    doc = yaml.safe_load(open(f"{d}/{wf}.yml"))
    jobs = doc["jobs"]
    res = jobs[rid]
    if res.get("name") != rname:
        bad.append(f"{wf}: result job name {res.get('name')!r}, want {rname!r}")
    if str(res.get("if", "")).replace(" ", "") != "${{always()}}":
        bad.append(f"{wf}: result job must run with always()")
    needs = res["needs"]
    sc = jobs.get("scope")
    if not sc or "ci/scope.sh" not in str(sc["steps"]):
        bad.append(f"{wf}: no scope job running ci/scope.sh")
        continue
    run = [s["run"] for s in res["steps"] if "run" in s][-1]
    m = re.search(r"ci/scope\.sh verdict (.*)", run)
    specs = {}
    for tok in (t for t in (m.group(1).split() if m else []) if not t.startswith("--")):
        j, k = tok.split("=", 1)
        specs[j] = k.strip("'")
    for j, k in specs.items():
        if j not in needs:
            bad.append(f"{wf}: verdict names {j} which the result job does not need")
        key = k.lstrip("!")
        jn = jobs[j].get("needs") or []
        jn = [jn] if isinstance(jn, str) else jn
        # A job behind a scoped job of the same key (a test partition after its build) is skipped
        # by that job's skip; the others carry the condition themselves.
        behind = any(specs.get(x) == k for x in jn)
        cond = str(jobs[j].get("if", ""))
        want = f"needs.scope.outputs.{key} " + ("== 'false'" if k.startswith("!") else "!= 'false'")
        if not behind and (want not in cond or "!cancelled()" not in cond):
            bad.append(f"{wf}: job {j} if={cond!r}, want !cancelled() and {want!r}")
        if not behind and "scope" not in jn:
            bad.append(f"{wf}: job {j} does not need scope")
        if key not in sc.get("outputs", {}):
            bad.append(f"{wf}: scope job has no output {key}")
    for j in needs:
        if j == "scope" or j in specs:
            continue
        # Unscoped jobs: the release build only (it keeps its own event rule).
        if not (wf == "windows" and j == "release"):
            bad.append(f"{wf}: needed job {j} is neither scoped nor the release build")
    # Every other job of the workflow is reached through the result job (directly or via needs).
    for j, spec in jobs.items():
        if j in (rid, "scope") or j in needs:
            continue
        n = spec.get("needs")
        n = [n] if isinstance(n, str) else (n or [])
        if not any(x in needs for x in n):
            bad.append(f"{wf}: job {j} is not covered by the result job")
for b in bad:
    print("FAIL:", b, file=sys.stderr)
sys.exit(1 if bad else 0)
PY
else
    echo "note: PyYAML missing, workflow wiring not checked" >&2
fi

# 8. The crate sets against the real graph (opt in: needs cargo; CI's Rust job turns it on).
if [ "${SCOPE_TEST_METADATA:-0}" = 1 ]; then
    meta="$tmp/meta.json"
    if (cd "$here/.." && cargo metadata --no-deps --offline --format-version 1 >"$meta" 2>"$tmp/meta.err"); then
        python3 -I - "$meta" "$scope" <<'PY' || fail=1
import json, re, sys
meta = json.load(open(sys.argv[1]))
src = open(sys.argv[2]).read()
pk = {p["name"]: p for p in meta["packages"]}
def closure(roots):
    seen, st = set(roots), list(roots)
    while st:
        for d in pk[st.pop()]["dependencies"]:
            if d["name"] in pk and d["name"] not in seen:
                seen.add(d["name"]); st.append(d["name"])
    return seen
def listed(var):
    return set(re.search(var + r'=" ([^"]*) "', src).group(1).split())
vm_roots = ["puddle-vm-tests", "puddle-compute-msb", "puddle-boot", "puddle-workspace", "puddle-lifecycle", "puddle-host", "puddle-agent"]
env = re.search(r"VM_CRATES: (.*)", open(sys.argv[2].replace("scope.sh", "../.github/workflows/vm-linux.yml")).read()).group(1)
want_vm = set(re.findall(r"-p (\S+)", env)) | {"puddle-agent"}
bad = []
if want_vm != set(vm_roots):
    bad.append(f"the VM crates in vm-linux.yml are {sorted(want_vm)}; scope-test.sh roots are {sorted(vm_roots)}")
for var, roots in (("NO_VM", vm_roots), ("NO_UI", ["puddle-e2e", "puddle-app"])):
    want = set(pk) - closure(roots)
    if listed(var) != want:
        bad.append(f"{var} is {sorted(listed(var))}; cargo metadata says {sorted(want)}")
dirs = {p["manifest_path"].split("/crates/")[-1].split("/")[0] for p in meta["packages"] if "/crates/" in p["manifest_path"]}
if dirs != set(pk):
    bad.append(f"crate directories {sorted(dirs)} differ from package names")
for b in bad:
    print("FAIL:", b, file=sys.stderr)
sys.exit(1 if bad else 0)
PY
    else
        bad "cargo metadata failed: $(cat "$tmp/meta.err")"
    fi
fi

if [ "$fail" = 0 ]; then
    echo "ok"
else
    exit 1
fi
