#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Build round: clean the crate, release build, release tests.
set -eu
# shellcheck source=/dev/null
. "$HOME/.cargo/env"
cd ripgrep
cargo clean -p ripgrep
cargo build --release --locked -q
cargo test --release --locked -q
