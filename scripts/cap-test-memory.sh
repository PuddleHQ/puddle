#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Runs a command with its heap limited, so one runaway test process dies by itself. nextest runs
# every test through this wrapper in the `mutants` profile (.config/nextest.toml).
#
# A mutant can turn a scanning loop into one that never advances while it pushes into a Vec; the
# process then takes about 200 MB a second until the run's timeout. Several of them at once
# filled a 32 GB machine in seconds. The limit makes the allocation fail, the test fail, and the
# mutant count as caught.
# The limit is soft (`-S`), so a child that is no test code (the real tools the login tests start) may
# lift it for itself: Claude Code's Bun runtime reserves about 1 GiB of address space and aborts under
# 512 MiB. A runaway test process cannot lift it: nothing in test code calls setrlimit.
# usage: cap-test-memory.sh <command> [args...]    PUDDLE_TEST_MEMORY_KB sets the limit (default 1 GiB).
set -eu
# glibc gives each thread that allocates its own arena and reserves 64 MiB of address space for it, and
# `ulimit -d` counts that reservation: a test that starts a few hundred blocking threads on a 40-core
# computer (the proxy's hostile tests) hit the limit with about 15 MB of heap and aborted, four runs in
# five. Two arenas keep the limit about the heap; a runaway Vec lives in the main arena either way.
export MALLOC_ARENA_MAX="${MALLOC_ARENA_MAX:-2}"
# Thread stacks count against `ulimit -d` too (private writable mappings, 2 MiB each for the threads
# Rust and tokio start). hostile_audit_backlog parks 300 blocking threads on purpose, which reaches
# about 450 MiB: at the old 512 MiB limit it passed 20 of 20 runs on a quiet 16-core computer, but a CI
# shard (4 vCPUs, other tests alongside) aborted it with `memory allocation of 14552 bytes failed`, and at
# 576 MiB it still failed 2 runs in 20. The default is 1 GiB (30 of 30 runs green on 4 CPUs); a runaway
# Vec still dies about five seconds after it starts. The tests that bound the heap (hostile_memory hg10,
# hostile_floods hg11) count with their own allocator and do not depend on this limit.
# shellcheck disable=SC3045 # `-d` is in dash and bash, which is every shell the Linux mutants job has.
ulimit -S -d "${PUDDLE_TEST_MEMORY_KB:-1048576}"
exec "$@"
