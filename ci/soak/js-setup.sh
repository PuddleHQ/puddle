#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# qa-soak-js: Node, pnpm and a clone of vite v5.4.11.
set -eu
sudo apt-get update -q
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends nodejs npm git ca-certificates
command -v pnpm >/dev/null 2>&1 || sudo npm install -g -q pnpm@9.15.0
# A pinned tag: vite's main needs a newer pnpm than the 9.15.0 above (its lockfile has several documents).
[ -d vite ] || git clone -q --depth 1 --branch v5.4.11 https://github.com/vitejs/vite
