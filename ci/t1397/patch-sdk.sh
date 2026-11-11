#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Clone the msb fork at the SDK tag, apply the trial patch, write a cargo config that patches every
# crate of that source to the clone. Usage: ci/t1397/patch-sdk.sh <dir>; prints the config path.
set -eu
dir=$1
tag=$(ci/msb-tag.sh)
git clone -q --depth 1 --branch "$tag" https://github.com/PuddleHQ/microsandbox "$dir/msb-src"
git -C "$dir/msb-src" apply "$PWD/ci/t1397/sdk-trial.patch"
cfg="$dir/patch.toml"
# Git Bash prints a Windows path with -W; cargo on Windows wants that form.
if src=$(cd "$dir/msb-src" && pwd -W 2>/dev/null); then :; else src=$(cd "$dir/msb-src" && pwd); fi
{
  echo '[patch."https://github.com/PuddleHQ/microsandbox"]'
  while read -r name path; do
    echo "$name = { path = \"$src/$path\" }"
  done <<LIST
microsandbox sdk/rust
microsandbox-agent-client packages/agent-client/rust
microsandbox-control-client packages/control-client/rust
microsandbox-db crates/db
microsandbox-filesystem crates/filesystem
microsandbox-image crates/image
microsandbox-metrics crates/metrics
microsandbox-migration crates/migration
microsandbox-network crates/network
microsandbox-protocol crates/protocol
microsandbox-protocol-client packages/protocol-client/rust
microsandbox-runtime crates/runtime
microsandbox-types packages/microsandbox-types/rust
microsandbox-types-macros packages/microsandbox-types/macros
microsandbox-utils crates/utils
LIST
} > "$cfg"
cat "$cfg" >&2
echo "$cfg"
