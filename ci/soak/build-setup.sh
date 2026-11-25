#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# qa-soak-build: Rust and a clone of ripgrep.
set -eu
sudo apt-get update -q
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends build-essential git curl ca-certificates
if ! command -v cargo >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/cargo" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
fi
[ -d ripgrep ] || git clone -q --depth 1 https://github.com/BurntSushi/ripgrep
