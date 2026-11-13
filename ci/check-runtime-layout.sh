#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Builds the Linux runtime folder from the msb that ci/fetch-msb.sh installed, through
# `cargo xtask runtime --msb`, and checks what the VM jobs' own layout leaves unchecked: the folder
# holds msb, libkrunfw.so.5 and the firmware under the versioned name pinned in
# ci/msb-runtime.sha256, and runtime.json says which version that is.
# Usage: ci/check-runtime-layout.sh <msb-dir> <scratch-dir>   (<scratch-dir> must not exist yet)
#        ci/check-runtime-layout.sh --check <runtime-dir> <version>
#
# The parts come from <msb-dir> and a fixture (no GitHub, no cargo-about): the firmware is the file
# fetch-msb.sh already verified, listed in a checksum list written here, and the fork facts and
# licence lists are placeholders. The real run, with the release and cargo-about on msb's tree,
# is not part of this check.
set -eu

# check <runtime-dir> <version>: the three files, and runtime.json's firmware version.
check() {
    for name in msb libkrunfw.so.5 "libkrunfw.so.$2" runtime.json; do
        [ -f "$1/$name" ] || { echo "runtime layout: $1 has no $name" >&2; return 1; }
    done
    [ -x "$1/msb" ] || { echo "runtime layout: $1/msb is not executable" >&2; return 1; }
    cmp -s "$1/libkrunfw.so.5" "$1/libkrunfw.so.$2" ||
        { echo "runtime layout: libkrunfw.so.5 and libkrunfw.so.$2 differ" >&2; return 1; }
    got=$(jq -r '.libkrunfw_version // "none"' "$1/runtime.json")
    [ "$got" = "$2" ] ||
        { echo "runtime layout: runtime.json libkrunfw_version is $got, pinned $2" >&2; return 1; }
    echo "runtime layout: ok ($1: msb, libkrunfw.so.5, libkrunfw.so.$2)"
}

if [ "${1:-}" = --check ] && [ "$#" -eq 3 ]; then
    check "$2" "$3"
    exit
fi
[ "$#" -eq 2 ] || { echo "usage: $0 <msb-dir> <scratch-dir>" >&2; exit 2; }
msb_dir=$1
scratch=$2
here=$(cd "$(dirname "$0")" && pwd)
cd "$here/.."

tag=$("$here/msb-tag.sh")
version=$("$here/fetch-msb.sh" --pinned "$tag" libkrunfw-linux-version)

# The firmware comes from the libkrunfw fork's release when the tag pins one (the xtask reads the
# same pin and refuses facts that name another release), else from upstream's.
if fork_tag=$("$here/fetch-msb.sh" --pinned "$tag" libkrunfw-linux-tag 2>/dev/null); then
    fw_dir=firmware fork_field=", \"fork_tag\": \"$fork_tag\""
else
    fw_dir=upstream fork_field=
fi
parts=$scratch/parts
mkdir -p "$parts/$fw_dir"
cp "$msb_dir/libkrunfw.so.5" "$parts/$fw_dir/libkrunfw-linux-x86_64.so"
(cd "$parts/$fw_dir" && sha256sum libkrunfw-linux-x86_64.so >checksums.sha256)
cat >"$parts/facts.json" <<JSON
{"fork_repo": "fixture/fork", "upstream_repo": "fixture/upstream", "tag": "$tag",
 "upstream_tag": "${tag%%-puddle.*}", "commits": [],
 "firmware": {"libkrunfw_version": "$version", "kernel_version": "0", "repo_url": "fixture", "commit": "0"$fork_field}}
JSON
for tree in msb puddle; do
    printf 'root v0.0.0 (/fixture)\nfixture-crate v1.0.0\n' >"$parts/$tree-tree.txt"
    cat >"$parts/$tree-about.json" <<'JSON'
{"licenses": [{"name": "MIT License", "id": "MIT", "text": "fixture",
  "used_by": [{"crate": {"name": "fixture-crate", "version": "1.0.0", "repository": null}, "path": ""}]}]}
JSON
done

cargo xtask runtime --tag "$tag" --from "$parts" --msb "$msb_dir/msb" --out "$scratch/runtime" --offline
check "$scratch/runtime" "$version"
