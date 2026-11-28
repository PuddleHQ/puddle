#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Keeps a data folder of a released Puddle as an upgrade fixture, so every later build must open it.
#
#   ci/capture-upgrade-fixture.sh <data-folder> <name> [<rule pattern> ...]
#
# Run it once at each release tag, on the data folder of that release's build after the host has
# stopped (a copy of a running database is torn), with the rule patterns the folder holds as
# arguments. It copies puddle.db, workspaces.json and settings/ into
# crates/puddle-host/tests/fixtures/upgrade/<name>/data/ and writes expected.json next to it (the
# workspace ids from the list, the rules you named). The fake_host test `upgrade_fixtures` opens
# every folder there. The msb home and a workspace's volume are not copied (a disk is too big to
# commit); they cross each release in the msi.yml `upgrade` job. Refuses a folder over 4 MiB.
# Self-test: ci/capture-upgrade-fixture-test.sh.
set -euo pipefail

data=${1:?usage: capture-upgrade-fixture.sh <data-folder> <name> [<rule pattern> ...]}
name=${2:?usage: capture-upgrade-fixture.sh <data-folder> <name> [<rule pattern> ...]}
shift 2
root=${UPGRADE_FIXTURES:-$(cd "$(dirname "$0")/.." && pwd)/crates/puddle-host/tests/fixtures/upgrade}
die() { echo "capture-upgrade-fixture: $*" >&2; exit 1; }

[[ $name =~ ^[A-Za-z0-9._-]+$ ]] || die "the name '$name' may hold letters, digits, '.', '_' and '-' only"
for f in puddle.db workspaces.json settings; do
    [ -e "$data/$f" ] || die "$data has no $f: is it a data folder?"
done
[ ! -e "$root/$name" ] || die "$root/$name exists: a kept folder is never overwritten"
size=$(du -sk "$data/puddle.db" "$data/workspaces.json" "$data/settings" | awk '{ s += $1 } END { print s }')
[ "$size" -le 4096 ] || die "the folder is ${size} KiB, over 4096: trim the audit log first"

mkdir -p "$root/$name/data"
cp "$data/puddle.db" "$data/workspaces.json" "$root/$name/data/"
cp -R "$data/settings" "$root/$name/data/settings"
rules=$(printf '%s\n' "$@" | jq -R . | jq -sc 'map(select(. != ""))')
jq -c --argjson rules "$rules" '{workspaces: [.workspaces[].id], rules: $rules}' "$data/workspaces.json" \
    >"$root/$name/expected.json"
echo "kept $root/$name ($(cat "$root/$name/expected.json"))"
