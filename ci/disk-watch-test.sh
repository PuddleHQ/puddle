#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of ci/disk-watch.sh with a free-space file in place of df, so each level is set by hand. Cases: a snapshot
# with room prints df and inodes and walks no directory (a `du` that records its calls proves it); the sizes mode,
# DISK_WATCH_SIZES=1 and a guard's marker each bring the size walk back; a snapshot under the warning level annotates and goes deep; a guard with
# room to spare stops no process and logs its samples; a guard under the floor annotates an error, writes the sizes
# and stops the processes of the step it was started for, grandchildren included (and nothing outside that tree, though the pattern matches it). Not seen: a real full disk or the runner's annotations.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
script="$here/disk-watch.sh"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/disk-watch-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fail=0
bad() { echo "disk-watch-test: FAIL: $*" >&2; fail=1; }
has() { grep -q -- "$2" "$1" || bad "$3: no '$2' in $1: $(head -c 600 "$1")"; }
# Poll (30 s deadline, 0.1 s steps) until a command succeeds: the test orders events on what the guard has logged, not on a delay.
until_ok() { local _; for _ in $(seq 1 300); do "$@" && return 0; sleep 0.1; done; return 1; }
# shellcheck disable=SC2329 # called through until_ok
samples_at_least() { local n; n=$(grep -c 'free=' "$1" 2>/dev/null || true); [ "${n:-0}" -ge "$2" ]; }
lacks() { ! grep -q -- "$2" "$1" || bad "$3: unexpected '$2' in $1"; }

mkdir -p "$tmp/rt/vmx" "$tmp/root"
head -c 60000000 /dev/zero >"$tmp/rt/vmx/big"
export RUNNER_TEMP="$tmp/rt" DISK_WATCH_ROOT="$tmp/root" DISK_WATCH_FREE_FILE="$tmp/free" GITHUB_STEP_SUMMARY="$tmp/sum"
export DISK_WATCH_INTERVAL=0.2 DISK_WATCH_WARN_GB=30 DISK_WATCH_FLOOR_GB=5

echo 80 >"$tmp/free"
: >"$tmp/sum"
"$script" snap "after the download" >"$tmp/out" 2>&1 || bad "snap exited non-zero"
has "$tmp/sum" '### disk: after the download' "snap summary"
has "$tmp/sum" 'Filesystem' "snap prints df"
has "$tmp/sum" 'Inodes' "snap prints inodes"
lacks "$tmp/sum" 'rt/vmx' "snap with room skips the size walk"
lacks "$tmp/sum" 'MiB' "snap with room lists no sizes"
lacks "$tmp/out" '::warning' "snap with room"
lacks "$tmp/sum" 'top level of' "snap with room stays shallow"

# No directory walk at all: a fake du on PATH records every call.
mkdir -p "$tmp/bin"
printf '#!/bin/sh\necho called >>"%s/du-calls"\nexec /usr/bin/du "$@"\n' "$tmp" >"$tmp/bin/du"
chmod +x "$tmp/bin/du"
: >"$tmp/sum"
PATH="$tmp/bin:$PATH" "$script" snap "no walk" >/dev/null 2>&1
[ ! -e "$tmp/du-calls" ] || bad "snap with room called du"

: >"$tmp/sum"
"$script" sizes "walk" >/dev/null 2>&1
has "$tmp/sum" 'rt/vmx' "the sizes mode lists the dirs over 50 MiB"
lacks "$tmp/sum" 'top level of' "the sizes mode with room stays shallow"
: >"$tmp/sum"
DISK_WATCH_SIZES=1 "$script" snap "forced" >/dev/null 2>&1
has "$tmp/sum" 'rt/vmx' "DISK_WATCH_SIZES=1 lists the dirs over 50 MiB"

echo 12 >"$tmp/free"
: >"$tmp/sum"
"$script" snap "low" >"$tmp/out" 2>&1
has "$tmp/out" '::warning title=runner disk low::low: 12 GB free' "low snap annotates"
has "$tmp/sum" 'top level of' "low snap goes deep"
has "$tmp/sum" 'rt/vmx' "low snap lists the dirs over 50 MiB"

echo 80 >"$tmp/free"
: >"$tmp/sum"
# The victim's argv carries this run's pid, so the pattern below matches it and nothing else on the machine: another
# run of this test, or any `sleep 300` (a pre-commit hook of another checkout), is not a target.
sleep "300.$$" &
victim=$!
"$script" guard roomy >"$tmp/out" 2>&1 &
g=$!
# Two samples: the loop ran past its first check with room to spare, and still stopped nothing.
until_ok samples_at_least "$tmp/rt/disk-watch-roomy.log" 2 || bad "the guard with room logged fewer than two samples in 30 s"
kill "$g"
wait "$g" 2>/dev/null
kill -0 "$victim" 2>/dev/null || bad "a guard with room stopped a process"
kill "$victim" 2>/dev/null; wait "$victim" 2>/dev/null
has "$tmp/rt/disk-watch-roomy.log" 'free=80GB' "guard samples"
has "$tmp/sum" 'disk guard roomy: free 80 GB now, lowest 80 GB' "guard end line"
lacks "$tmp/out" '::error' "guard with room"
[ ! -e "$tmp/rt/disk-watch-low" ] || bad "a guard with room left the low-disk marker"

# The step is a shell that starts the guard and the heavy processes (its children). A foreign process outside that
# tree (`setsid sleep`, as another job on a shared host or a pre-commit hook would run) carries an argv the pattern
# also matches, and must survive. Its argv ends in this run's pid, so another run of this test is no target either.
setsid sleep "301.$$" &
foreign=$!
cat >"$tmp/step.sh" <<EOS
#!/usr/bin/env bash
"$script" guard full >"$tmp/out" 2>&1 &
sleep "300.$$" &
bash -c 'sleep "302.$$" & wait' &
wait
EOS
chmod +x "$tmp/step.sh"
DISK_WATCH_KILL='sleep 30[012][.]'"$$"'$' "$tmp/step.sh" &
step=$!
until_ok pgrep -f "sleep 300[.]$$" >/dev/null || bad "the step's process did not start"
until_ok pgrep -f "sleep 302[.]$$" >/dev/null || bad "the step's grandchild did not start"
until_ok samples_at_least "$tmp/rt/disk-watch-full.log" 1 || bad "the guard under test logged no sample in 30 s"
echo 2 >"$tmp/free"
for _ in $(seq 1 300); do kill -0 "$step" 2>/dev/null || break; sleep 0.2; done
if kill -0 "$step" 2>/dev/null; then bad "the step did not end after the guard stopped its processes"; kill "$step"; fi
pgrep -f "sleep 300[.]$$" >/dev/null && { bad "the guard under the floor left the step's process running"; pkill -f "sleep 300[.]$$"; }
pgrep -f "sleep 302[.]$$" >/dev/null && { bad "the guard under the floor left the step's grandchild running"; pkill -f "sleep 302[.]$$"; }
kill -0 "$foreign" 2>/dev/null || bad "the guard stopped a process outside its step's tree"
kill "$foreign" 2>/dev/null
has "$tmp/out" '::error title=runner disk full::full: 2 GB free' "floor annotation"
has "$tmp/sum" 'under the floor' "floor snapshot in the summary"
has "$tmp/sum" 'lowest 2 GB' "floor end line"

# The guard that saw the floor left its marker: a later snapshot walks the sizes though the free space is back.
[ -e "$tmp/rt/disk-watch-low" ] || bad "the guard under the floor left no low-disk marker"
echo 80 >"$tmp/free"
: >"$tmp/sum"
"$script" snap "after the low guard" >/dev/null 2>&1
has "$tmp/sum" 'rt/vmx' "a snapshot after a low guard walks the sizes"

[ "$fail" = 0 ] && echo "disk-watch-test: ok"
exit "$fail"
