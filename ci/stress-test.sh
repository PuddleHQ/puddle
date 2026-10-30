#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of the nightly stress tooling with synthetic reports and stub tools: the three failure parsers of
# flaky-report.py --stress, the row and the report of stress-yield.py (a known fixed flake is counted as known,
# a new one as new, a run with no report as no-report), and ci/stress.sh itself (busy loops run during the tool
# and are gone after it, the tool's status is the script's, result.env names the report). Not seen: the real
# nextest, Playwright and vitest (the commit that added this file ran the real nextest against the commit before a slowloris fix), and GitHub.
set -euo pipefail
command -v taskset >/dev/null || { echo "stress-test: skipped, taskset is Linux only (the stress job runs on ubuntu)"; exit 0; }
# The CPUs the test may pin to are the ones this process may use (a CI lane is not allowed all of them).
cpu=$(taskset -cp $$ | sed 's/.*: //; s/[-,].*//')
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/stress-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail() { echo "stress-test: $*" >&2; exit 1; }
FLAKY="$here/flaky-report.py"
bt='`' # markdown code ticks, for the patterns below

# 1. flaky-report.py --stress (its counts also feed stress-yield.py)
cat >"$tmp/nextest.xml" <<'XML'
<testsuites><testsuite name="a@stress-0"><testcase name="steady" classname="p::bin"/>
<testcase name="a_proxy_that_turns_the_sign_in_down_is_one_event" classname="p::fake_host"/></testsuite>
<testsuite name="a@stress-1"><testcase name="steady" classname="p::bin"/>
<testcase name="a_proxy_that_turns_the_sign_in_down_is_one_event" classname="p::fake_host"><failure message="x"/></testcase></testsuite>
<testsuite name="a@stress-2"><testcase name="steady" classname="p::bin"><error message="y"/></testcase>
<testcase name="a_proxy_that_turns_the_sign_in_down_is_one_event" classname="p::fake_host"/></testsuite></testsuites>
XML
python3 "$FLAKY" --stress rust "$tmp/nextest.xml" >"$tmp/md" 2>"$tmp/warn"
grep -q "^- ${bt}p::bin::steady${bt}: failed 1 of 3 runs\$" "$tmp/md" || fail "nextest: steady (an error element): $(cat "$tmp/md")"
grep -q "^- ${bt}p::fake_host::a_proxy_that_turns_the_sign_in_down_is_one_event${bt}: failed 1 of 3 runs\$" "$tmp/md" || fail "nextest: sign-in"
grep -q '2 tests, 6 runs' "$tmp/md" || fail "nextest: totals"
[ "$(grep -c '^::warning' "$tmp/warn")" = 2 ] || fail "nextest: one warning per failing test"

cat >"$tmp/pw.json" <<'JSON'
{"suites":[{"suites":[{"specs":[{"title":"opens","file":"a.spec.ts","tests":[
 {"projectName":"webkit","results":[{"status":"passed"},{"status":"timedOut"},{"status":"passed"}]},
 {"projectName":"chromium","results":[{"status":"passed"},{"status":"passed"}]}]}]}]}]}
JSON
python3 "$FLAKY" --stress playwright "$tmp/pw.json" 2>/dev/null | grep -q "^- ${bt}a.spec.ts > opens \\[webkit\\]${bt}: failed 1 of 3 runs\$" || fail "playwright: webkit repeat"
python3 "$FLAKY" --stress playwright "$tmp/pw.json" 2>/dev/null | grep -q 'chromium' && fail "playwright: a passing project must not be listed"

cat >"$tmp/vt.xml" <<'XML'
<testsuites><testsuite name="s"><testcase classname="f.test.ts" name="t1"/><testcase classname="f.test.ts" name="t1"><failure/></testcase></testsuite></testsuites>
XML
python3 "$FLAKY" --stress vitest "$tmp/vt.xml" 2>/dev/null | grep -q "^- ${bt}f.test.ts::t1${bt}: failed 1 of 2 runs\$" || fail "vitest"
python3 "$FLAKY" --stress rust "$tmp/missing.xml" | grep -q 'No report' || fail "a missing report must say so"

# 2. stress-yield.py: rows and the report
printf '# c\nsign_in_down\tfix a\n' >"$tmp/known.tsv"
mkdir -p "$tmp/out/rust" "$tmp/out/vitest" "$tmp/out/playwright"
cp "$tmp/nextest.xml" "$tmp/out/rust/report.xml"
printf 'KIND=rust\nEXIT=100\nWALL_S=600\nREPORT=report.xml\nARGS=x\n' >"$tmp/out/rust/result.env"
cp "$tmp/vt.xml" "$tmp/out/vitest/report.xml"
printf 'KIND=vitest\nEXIT=1\nWALL_S=60\nREPORT=report.xml\nARGS=x\n' >"$tmp/out/vitest/result.env"
printf 'KIND=playwright\nEXIT=1\nWALL_S=5\nREPORT=report.json\nARGS=x\n' >"$tmp/out/playwright/result.env"
for k in rust vitest playwright; do
    python3 "$here/stress-yield.py" record "$tmp/out" "$k" --sha abc --run-id 7 --job-seconds 1800 --date 2026-10-30 --known "$tmp/known.tsv" >/dev/null
done
tsv="$tmp/out/yield.tsv"
[ "$(wc -l <"$tsv")" = 4 ] || fail "yield.tsv: header and three rows, got $(wc -l <"$tsv")"
awk -F'\t' '$4=="rust" && $5=="failed" && $8==2 && $9==1 && $7=="30.0"' "$tsv" | grep -q . || fail "rust row: $(cat "$tsv")"
awk -F'\t' '$4=="vitest" && $5=="failed" && $8==1 && $9==0' "$tsv" | grep -q . || fail "vitest row"
awk -F'\t' '$4=="playwright" && $5=="no-report"' "$tsv" | grep -q . || fail "a run with no report must be no-report"
# a report with no test runs in it is not a green night
mkdir -p "$tmp/empty/vitest"
printf '<testsuites/>' >"$tmp/empty/vitest/report.xml"
printf 'KIND=vitest\nEXIT=0\nWALL_S=1\nREPORT=report.xml\nARGS=x\n' >"$tmp/empty/vitest/result.env"
python3 "$here/stress-yield.py" record "$tmp/empty" vitest --sha a --run-id 1 --job-seconds 60 --date 2026-10-30 --known "$tmp/known.tsv" >/dev/null
awk -F'\t' 'NR==2 && $5=="no-report" && $12==0' "$tmp/empty/yield.tsv" | grep -q . || fail "an empty report must be no-report: $(cat "$tmp/empty/yield.tsv")"
python3 "$here/stress-yield.py" report "$tsv" "$tsv" >"$tmp/report.md"
grep -q '^| rust | 1 | 2 | 2 | 0 | 4 | 2 | 2 |' "$tmp/report.md" || fail "report row for rust: $(cat "$tmp/report.md")"
grep -q '1 nights' "$tmp/report.md" || fail "report: nights"

# 3. stress.sh with stub tools on PATH
mkdir -p "$tmp/bin" "$tmp/repo/ci" "$tmp/repo/ui"
cp "$here/stress.sh" "$tmp/repo/ci/stress.sh"
cat >"$tmp/bin/cargo" <<'STUB'
#!/bin/sh
# `cargo nextest run ... --no-run` builds; the stress run is the one with --stress-count.
case " $* " in *" --no-run "*) exit 0 ;; esac
echo "$*" >"$STUB_DIR/cargo.args"
# The loops are children that may not have reached their command line yet: wait for them (5 s).
for _ in $(seq 50); do
    pgrep -P "$PPID" -f 'while :' >"$STUB_DIR/busy.pids" || true
    [ "$(wc -l <"$STUB_DIR/busy.pids")" -ge "$STUB_EXPECT" ] && break
    sleep 0.1
done
mkdir -p target/nextest/ci
cp "$STUB_DIR/report.xml" target/nextest/ci/junit.xml
exit "${STUB_EXIT:-0}"
STUB
chmod +x "$tmp/bin/cargo"
export STUB_DIR="$tmp" PATH="$tmp/bin:$PATH"
cp "$tmp/nextest.xml" "$tmp/report.xml"
(cd "$tmp/repo" && STRESS_CPUS=$cpu STRESS_BUSY=2 STRESS_COUNT=3 STUB_EXIT=100 STUB_EXPECT=2 ci/stress.sh rust) && fail "the tool's failing status must be the script's"
[ "$(wc -l <"$tmp/busy.pids")" = 2 ] || fail "two busy loops (STRESS_BUSY) must run during the tool: $(cat "$tmp/busy.pids")"
# kill is asynchronous: give a loop up to 5 s to go before calling it a leak.
while read -r pid; do
    for _ in $(seq 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    ! kill -0 "$pid" 2>/dev/null || fail "busy loop $pid outlived the script"
done <"$tmp/busy.pids"
grep -q -- '--stress-count 3' "$tmp/cargo.args" || fail "stress count not passed: $(cat "$tmp/cargo.args")"
grep -q '^EXIT=100$' "$tmp/repo/target/stress/rust/result.env" || fail "result.env lacks the exit status"
[ -s "$tmp/repo/target/stress/rust/report.xml" ] || fail "the junit report was not collected"
# playwright and vitest: the tool gets its repeat and report flags and runs pinned to STRESS_CPUS.
cat >"$tmp/bin/npx" <<'STUB'
#!/bin/sh
echo "$*" >"$STUB_DIR/npx.args"
env >"$STUB_DIR/npx.env"
taskset -cp $$ >"$STUB_DIR/npx.affinity"
exit 0
STUB
chmod +x "$tmp/bin/npx"
(cd "$tmp/repo" && STRESS_CPUS=$cpu STRESS_BUSY=1 STRESS_PROJECTS=webkit ci/stress.sh playwright)
for want in '--repeat-each 3' '--project webkit'; do grep -q -- "$want" "$tmp/npx.args" || fail "playwright args lack $want: $(cat "$tmp/npx.args")"; done
grep -q '^PLAYWRIGHT_JSON_OUTPUT_NAME=.*/stress/playwright/report.json$' "$tmp/npx.env" || fail "playwright json report path"
grep -q '^PUDDLE_E2E_PROJECTS=webkit$' "$tmp/npx.env" || fail "playwright warm-up must be narrowed to the project: $(grep PUDDLE_E2E "$tmp/npx.env")"
grep -q '^PUDDLE_E2E_CPU_THROTTLE=4$' "$tmp/npx.env" || fail "playwright throttle"
grep -q "affinity list: $cpu\$" "$tmp/npx.affinity" || fail "playwright must run pinned to STRESS_CPUS: $(cat "$tmp/npx.affinity")"
(cd "$tmp/repo" && STRESS_CPUS=$cpu STRESS_BUSY=1 ci/stress.sh vitest)
for want in '--sequence.shuffle' '--repeats 5' '--outputFile.junit=.*/stress/vitest/report.xml'; do grep -q -- "$want" "$tmp/npx.args" || fail "vitest args lack $want: $(cat "$tmp/npx.args")"; done
grep -q "affinity list: $cpu\$" "$tmp/npx.affinity" || fail "vitest must run pinned"
# The rust default is one CPU with 6 busy loops, and a partition reaches nextest.
(cd "$tmp/repo" && STRESS_CPUS=$cpu STRESS_PARTITION=2/3 STUB_EXPECT=6 ci/stress.sh rust) || fail "a passing tool must pass the script"
[ "$(wc -l <"$tmp/busy.pids")" = 6 ] || fail "the rust default is 6 busy loops: $(cat "$tmp/busy.pids")"
grep -q -- '--partition count:2/3' "$tmp/cargo.args" || fail "partition not passed: $(cat "$tmp/cargo.args")"
(cd "$tmp/repo" && ci/stress.sh nonsense 2>/dev/null) && fail "an unknown kind must fail"
echo "stress-test: ok"
