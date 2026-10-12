#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Self-test of ci/check-runtime-layout.sh --check: a complete folder passes, and each way a folder
# can fall short (missing file, wrong firmware copy, wrong or missing version in runtime.json,
# msb not executable) fails with its own message. Not seen: the xtask run itself.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
command -v jq >/dev/null 2>&1 || { echo "check-runtime-layout-test: skipped (no jq)"; exit 0; }
tmp=$(mktemp -d "${TMPDIR:-/tmp}/check-runtime-layout-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# folder <name>: a good folder for version 5.6.1.
folder() {
    mkdir "$tmp/$1"
    printf '#!/bin/sh\n' >"$tmp/$1/msb"
    chmod +x "$tmp/$1/msb"
    printf fw >"$tmp/$1/libkrunfw.so.5"
    printf fw >"$tmp/$1/libkrunfw.so.5.6.1"
    echo '{"libkrunfw_version": "5.6.1"}' >"$tmp/$1/runtime.json"
}
# expect <folder> <message part>: the check fails and says <message part>.
expect() {
    if out=$("$here/check-runtime-layout.sh" --check "$tmp/$1" 5.6.1 2>&1); then
        echo "check-runtime-layout-test: $1 passed, expected: $2" >&2
        exit 1
    fi
    case "$out" in
    *"$2"*) ;;
    *) echo "check-runtime-layout-test: $1: expected '$2', got: $out" >&2; exit 1 ;;
    esac
}

folder good
"$here/check-runtime-layout.sh" --check "$tmp/good" 5.6.1 >/dev/null

folder no-versioned
rm "$tmp/no-versioned/libkrunfw.so.5.6.1"
expect no-versioned "has no libkrunfw.so.5.6.1"

folder no-soname
rm "$tmp/no-soname/libkrunfw.so.5"
expect no-soname "has no libkrunfw.so.5"

folder not-executable
chmod -x "$tmp/not-executable/msb"
expect not-executable "is not executable"

folder differing
printf other >"$tmp/differing/libkrunfw.so.5.6.1"
expect differing "differ"

folder old-version
echo '{"libkrunfw_version": "5.6.0"}' >"$tmp/old-version/runtime.json"
expect old-version "is 5.6.0, pinned 5.6.1"

folder no-version
echo '{}' >"$tmp/no-version/runtime.json"
expect no-version "is none, pinned 5.6.1"

echo "check-runtime-layout-test: ok"
