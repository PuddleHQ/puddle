#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
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
# usage: cap-test-memory.sh <command> [args...]    PUDDLE_TEST_MEMORY_KB sets the limit (default 512 MiB).
set -eu
# glibc gives each thread that allocates its own arena and reserves 64 MiB of address space for it, and
# `ulimit -d` counts that reservation: a test that starts a few hundred blocking threads on a 40-core
# computer (the proxy's hostile tests) hit the limit with about 15 MB of heap and aborted, four runs in
# five. Two arenas keep the limit about the heap; a runaway Vec lives in the main arena either way.
export MALLOC_ARENA_MAX="${MALLOC_ARENA_MAX:-2}"
# shellcheck disable=SC3045 # `-d` is in dash and bash, which is every shell the Linux mutants job has.
ulimit -S -d "${PUDDLE_TEST_MEMORY_KB:-524288}"
exec "$@"
