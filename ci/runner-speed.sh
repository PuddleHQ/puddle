#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Two timings of the runner it runs on, printed on one line: hashing 1 GiB of zeroes (one core) and writing
# 256 MiB with fsync (disk). Evidence only: vm-linux prints it above the tests so that a run where every test
# ran several times slower than usual names the cause (a slow host) instead of looking like a hang. It gates
# nothing and exits 0 whatever it measures.
set -uo pipefail

secs() { # <command...>: wall seconds, to one decimal
    local t0 t1
    t0=$(date +%s.%N)
    "$@" >/dev/null 2>&1
    t1=$(date +%s.%N)
    awk -v a="$t0" -v b="$t1" 'BEGIN { printf "%.1f", b - a }'
}

# shellcheck disable=SC2329 # run through secs
hash_zeroes() { head -c 1073741824 /dev/zero | sha256sum; }
# shellcheck disable=SC2329
write_file() { dd if=/dev/zero of="$1" bs=1M count=256 conv=fsync; }

scratch="${RUNNER_TEMP:-${TMPDIR:-/tmp}}/runner-speed.disk"
cpu_s=$(secs hash_zeroes)
disk_s=$(secs write_file "$scratch")
: >"$scratch" # empties the file (the runner is thrown away, nothing to delete)
echo "runner speed: sha256 of 1 GiB ${cpu_s}s, 256 MiB written with fsync ${disk_s}s, $(nproc) cores"
exit 0
