#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/disk-watch.sh with a free-space file in place of df, so each level is set by hand. Cases: a snapshot
# lists the dirs and writes the job summary; a snapshot under the warning level annotates and goes deep; a guard with
# room to spare stops no process and logs its samples; a guard under the floor annotates an error, writes the sizes
# and stops the process the pattern names (and nothing else). Not seen: a real full disk or the runner's annotations.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/disk-watch.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/disk-watch-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "disk-watch-test: FAIL: $*" >&2; fail=1; }
has() { grep -q -- "$2" "$1" || bad "$3: no '$2' in $1: $(head -c 600 "$1")"; }
lacks() { ! grep -q -- "$2" "$1" || bad "$3: unexpected '$2' in $1"; }

mkdir -p "$tmp/rt/vmx" "$tmp/root"
head -c 60000000 /dev/zero >"$tmp/rt/vmx/big"
export RUNNER_TEMP="$tmp/rt" DISK_WATCH_ROOT="$tmp/root" DISK_WATCH_FREE_FILE="$tmp/free" GITHUB_STEP_SUMMARY="$tmp/sum"
export DISK_WATCH_INTERVAL=0.2 DISK_WATCH_WARN_GB=30 DISK_WATCH_FLOOR_GB=5

echo 80 >"$tmp/free"
: >"$tmp/sum"
"$script" snap "after the download" >"$tmp/out" 2>&1 || bad "snap exited non-zero"
has "$tmp/sum" '### disk: after the download' "snap summary"
has "$tmp/sum" 'rt/vmx' "snap lists the dirs over 50 MiB"
lacks "$tmp/out" '::warning' "snap with room"
lacks "$tmp/sum" 'top level of' "snap with room stays shallow"

echo 12 >"$tmp/free"
: >"$tmp/sum"
"$script" snap "low" >"$tmp/out" 2>&1
has "$tmp/out" '::warning title=runner disk low::low: 12 GB free' "low snap annotates"
has "$tmp/sum" 'top level of' "low snap goes deep"

echo 80 >"$tmp/free"
: >"$tmp/sum"
sleep 300 &
victim=$!
"$script" guard roomy >"$tmp/out" 2>&1 &
g=$!
sleep 1
kill "$g"
wait "$g" 2>/dev/null
kill -0 "$victim" 2>/dev/null || bad "a guard with room stopped a process"
has "$tmp/rt/disk-watch-roomy.log" 'free=80GB' "guard samples"
has "$tmp/sum" 'disk guard roomy: free 80 GB now, lowest 80 GB' "guard end line"
lacks "$tmp/out" '::error' "guard with room"

# The pattern names the victim by its argv; the guard itself must survive it.
DISK_WATCH_KILL='sleep 3[0]0$' "$script" guard full >"$tmp/out" 2>&1 &
g=$!
sleep 0.5
echo 2 >"$tmp/free"
for _ in $(seq 1 300); do kill -0 "$g" 2>/dev/null || break; sleep 0.2; done
kill -0 "$g" 2>/dev/null && { bad "the guard did not end after stopping the step"; kill "$g"; }
kill -0 "$victim" 2>/dev/null && { bad "the guard under the floor left the process running"; kill "$victim"; }
has "$tmp/out" '::error title=runner disk full::full: 2 GB free' "floor annotation"
has "$tmp/sum" 'under the floor' "floor snapshot in the summary"
has "$tmp/sum" 'lowest 2 GB' "floor end line"

[ "$fail" = 0 ] && echo "disk-watch-test: ok"
exit "$fail"
