#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Usage: ci/boot-sh-root.sh
# Runs guest/boot.sh's root-only paths (crates/puddle-boot/tests/boot_sh_root.rs): the real `chown` calls against a
# fake root folder, ownership read back with `stat`. The test binary is built as the current user, so root never
# writes into `target/`, and run under `sudo` with one thread (the hook scans `/proc` for its own processes).
set -euo pipefail
sudo -n true
bin=$(cargo test --locked -p puddle-boot --test boot_sh_root --no-run --message-format=json |
    jq -r 'select(.executable != null).executable')
test -x "$bin"
tmp=$(mktemp -d)
sudo -n env PUDDLE_REQUIRE_ROOT=1 TMPDIR="$tmp" "$bin" --test-threads=1
