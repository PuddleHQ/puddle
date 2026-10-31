#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Runs a command with its heap limited, so one runaway test process dies by itself. nextest runs
# every test through this wrapper in the `mutants` profile (.config/nextest.toml).
#
# A mutant can turn a scanning loop into one that never advances while it pushes into a Vec; the
# process then takes about 200 MB a second until the run's timeout. Several of them at once
# filled a 32 GB machine in seconds. The limit makes the allocation fail, the test fail, and the
# mutant count as caught.
# usage: cap-test-memory.sh <command> [args...]    PUDDLE_TEST_MEMORY_KB sets the limit (default 2 GiB).
set -eu
# shellcheck disable=SC3045 # `-d` is in dash and bash, which is every shell the Linux mutants job has.
ulimit -d "${PUDDLE_TEST_MEMORY_KB:-2097152}"
exec "$@"
