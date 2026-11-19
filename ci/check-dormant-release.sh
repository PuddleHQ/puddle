#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# The shipped build keeps `puddle serve` and the mise layer dormant: they are cargo features
# (`serve`, `mise`) that are off by default, so a release build must not turn them on. This
# fails when a command in the release pipeline (the CI scripts and workflows, `mise.toml` and `.mise.toml`) builds
# a release artifact (`--release`, `cargo tauri build`, `cargo tauri bundle`) and passes
# `--all-features` or a feature list naming `serve` or `mise`. Backslash-continued lines count
# as one command. Usage: ci/check-dormant-release.sh [root]  (root defaults to the repository).
set -eu

root=${1:-$(cd "$(dirname "$0")/.." && pwd)}
found=0
for f in "$root"/ci/*.sh "$root"/.github/workflows/*.yml "$root"/mise.toml "$root"/.mise.toml; do
    [ -f "$f" ] || continue
    case $f in *-test.sh) continue ;; esac
    out=$(awk -v file="${f#"$root"/}" '
        function check(cmd, n) {
            if (cmd !~ /(cargo|tauri)/) return
            if (cmd !~ /(--release|tauri +build|tauri +bundle)/) return
            if (cmd ~ /--all-features/ || cmd ~ /(--features|--feature|-F)[ =]+[^ ]*(serve|mise)/)
                printf "%s:%d: a release build turns on a dormant feature: %s\n", file, n, cmd
        }
        {
            line = $0
            if (cont == "") start = NR
            if (line ~ /\\[ \t]*$/) { sub(/\\[ \t]*$/, "", line); cont = cont line " "; next }
            check(cont line, start)
            cont = ""
        }
        END { if (cont != "") check(cont, start) }
    ' "$f")
    if [ -n "$out" ]; then
        printf '%s\n' "$out" >&2
        found=1
    fi
done
if [ "$found" -ne 0 ]; then
    echo "check-dormant-release: the shipped build must not enable the serve or mise features (docs/serve.md)" >&2
    exit 1
fi
echo "check-dormant-release: no release build enables the serve or mise features"
