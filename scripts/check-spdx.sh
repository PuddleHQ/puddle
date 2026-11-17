#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Every tracked source file must carry an SPDX licence identifier in its first two lines (the
# second allows for a shebang), and the identifier must be the licence of the file's crate (from
# `cargo metadata`), or of the workspace root for a file outside every crate. Generated files, lock
# files and documents are exempt (CONTRIBUTING.md, "Licence headers").
#
# Files that come from elsewhere keep their own identifier: the folders in `own_licence` below.
# Add a folder only with a licence file or a written reason beside it.
set -eu
cd "$(dirname "$0")/.."

# Folders whose files keep their own licence. ci/msb-repros is the Puddle fork's own test material
# for microsandbox (Apache-2.0, the fork's licence); vendor/ holds third-party crates.
own_licence='^(vendor|ci/msb-repros)/'

root=$(pwd)
# One "<crate dir relative to the root><TAB><licence>" line per workspace member; "." is the root.
# The output does not depend on the toolchain, so a machine without the pinned one (the light CI
# job) falls back to stable.
metadata() {
    cargo metadata --no-deps --format-version 1 --offline 2>/dev/null ||
        RUSTUP_TOOLCHAIN=stable cargo metadata --no-deps --format-version 1 --offline
}
crates=$(metadata | python3 -I -c '
import json, os, sys
root = sys.argv[1]
for p in json.load(sys.stdin)["packages"]:
    if p["license"] is None:
        sys.exit("crate %s has no licence in cargo metadata" % p["name"])
    print("%s\t%s" % (os.path.relpath(os.path.dirname(p["manifest_path"]), root), p["license"]))
' "$root")
root_licence=$(printf '%s\n' "$crates" | awk -F'\t' '$1 == "." { print $2 }')
if [ -z "$root_licence" ]; then
    root_licence=$(sed -n 's/^license = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
fi
[ -n "$root_licence" ] || { echo "no workspace licence found" >&2; exit 1; }

# The licence a file must carry: the longest crate dir that is a prefix of the path, else the root's.
expected_for() {
    printf '%s\n' "$crates" | awk -F'\t' -v f="$1" -v def="$root_licence" '
        $1 != "." && index(f, $1 "/") == 1 && length($1) > best { best = length($1); lic = $2 }
        END { print (best ? lic : def) }'
}

missing=0
for f in $(git ls-files -- '*.rs' '*.toml' '*.sh' '*.ps1' '*.yml' '*.yaml' '*.ts' '*.js' \
    '*.svelte' '*.html' '*.css' '.githooks/*' '.config/*'); do
    [ -f "$f" ] || continue
    line=$(head -n 2 "$f" | grep 'SPDX-License-Identifier:' | head -n 1 || true)
    if [ -z "$line" ]; then
        echo "missing SPDX header: $f" >&2
        missing=1
        continue
    fi
    if printf '%s\n' "$f" | grep -Eq "$own_licence"; then
        continue
    fi
    found=$(printf '%s\n' "$line" | tr -d '\r' | sed 's/.*SPDX-License-Identifier:[[:space:]]*//; s/[[:space:]]*\(-->\|\*\/\)[[:space:]]*$//')
    want=$(expected_for "$f")
    if [ "$found" != "$want" ]; then
        echo "wrong SPDX identifier: $f says $found, its crate is $want" >&2
        missing=1
    fi
done
[ "$missing" -eq 0 ] && echo "SPDX headers ok"
exit "$missing"
