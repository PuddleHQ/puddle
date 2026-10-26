#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Builds Puddle's per-user MSI on Windows (Git Bash), in three steps so a second MSI (the upgrade
# test's newer version) neither stages nor compiles again.
#
#   ci/build-msi.sh stage <stage-dir>
#       Everything the installer carries besides the shell itself, in <stage-dir> (which must not
#       exist): runtime/ (the pinned msb and firmware from `cargo xtask runtime`, the guest agent,
#       the code-server release, licences and third-party notices) and bin/ (the `puddle` command
#       line). Needs the UI built (`cd ui && npm ci && npm run build`: the shell embeds it), `gh`
#       and `cargo-about` (see `cargo xtask`), network.
#   ci/build-msi.sh shell <stage-dir>
#       Builds the shell in release mode (the slow step). Needs `cargo tauri` on PATH
#       (ci/fetch-tauri-cli.sh).
#   ci/build-msi.sh bundle <stage-dir> <out-dir> [<version>]
#       Has Tauri's WiX bundler pack the built shell with the staged files into
#       <out-dir>/Puddle_<version>_x64_en-US.msi, from the template in crates/puddle-app/wix/.
#       <version> is numeric `major.minor.patch` and defaults to the workspace's. The program's own
#       version resource stays the workspace's whatever <version> is.
#
# Why the staged files are named in a generated overlay and not in tauri.conf.json: Tauri checks
# every resource and sidecar path on each build of the crate, so listing them in the committed file
# would break every ordinary `cargo build`. The overlay is only read by the bundling run.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
app=$root/crates/puddle-app
triple=x86_64-pc-windows-msvc

fail() { echo "build-msi: $*" >&2; exit 1; }
case "$(uname -s)" in
MINGW* | MSYS* | CYGWIN*) ;;
*) fail "the MSI builds on Windows only (WiX), not on $(uname -s)" ;;
esac

target_dir() {
    if [ -n "${CARGO_TARGET_DIR:-}" ]; then
        (cd "$CARGO_TARGET_DIR" && pwd)
    else
        echo "$root/target"
    fi
}

stage() {
    [ "$#" -eq 1 ] || fail "usage: $0 stage <stage-dir>"
    stage=$1
    [ ! -e "$stage" ] || fail "$stage exists"
    [ -f "$root/ui/build/index.html" ] || fail "the UI is not built: cd ui && npm ci && npm run build"
    cd "$root"
    mkdir -p "$stage/bin"
    cargo xtask runtime --out "$stage/runtime"
    cargo xtask code-server --out "$stage/runtime/code-server" --licenses "$stage/runtime/licenses"
    agent=$("$here/build-agent.sh" --locked)
    cp "$agent" "$stage/runtime/puddle-agent"
    cp "$root/ui/THIRD-PARTY-NOTICES.txt" "$stage/runtime/licenses/THIRD-PARTY-ui.txt"
    cp "$root/LICENSE" "$stage/runtime/licenses/GPL-3.0.txt"
    cargo build --release --locked --manifest-path "$root/crates/puddle/Cargo.toml"
    # Tauri names a sidecar `<name>-<target triple>` and installs it beside the shell as `<name>`.
    cp "$(target_dir)/release/puddle.exe" "$stage/bin/puddle-$triple.exe"
    echo "build-msi: staged in $stage"
}

# overlay <stage-dir> <version>: writes the overlay config and prints its path.
overlay() {
    stage=$1
    # Paths relative to tauri.conf.json: Tauri resolves a resource folder against its own pattern.
    rel=$(realpath --relative-to="$app" "$stage")
    cat >"$stage/tauri.msi.conf.json" <<JSON
{
  "version": "$2",
  "bundle": {
    "active": true,
    "resources": { "$rel/runtime": "runtime" },
    "externalBin": ["$rel/bin/puddle"]
  }
}
JSON
    echo "$stage/tauri.msi.conf.json"
}

staged() {
    [ -d "$1/runtime" ] && [ -f "$1/bin/puddle-$triple.exe" ] || fail "$1 is not a staged folder (run '$0 stage' first)"
}

workspace_version() { awk -F'"' '/^version = / { print $2; exit }' "$root/Cargo.toml"; }

# The release build of the shell, without packing it: the slow step, done once.
shell() {
    [ "$#" -eq 1 ] || fail "usage: $0 shell <stage-dir>"
    staged "$1"
    stage=$(cd "$1" && pwd)
    cfg=$(overlay "$stage" "$(workspace_version)")
    cd "$app"
    cargo tauri build --no-bundle --ci --config "$cfg" -- --locked
}

# Packs the built shell and the staged files; one run per version, none rebuilds the program.
bundle() {
    [ "$#" -ge 2 ] && [ "$#" -le 3 ] || fail "usage: $0 bundle <stage-dir> <out-dir> [<version>]"
    staged "$1"
    stage=$(cd "$1" && pwd)
    out=$2
    version=${3:-$(workspace_version)}
    case "$version" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "version '$version' is not numeric major.minor.patch" ;;
    esac
    mkdir -p "$out"
    cfg=$(overlay "$stage" "$version")
    # Tauri's output name has the version; a previous build of another version must not be taken for this one.
    target=$(target_dir)
    rm -f "$target"/release/bundle/msi/*.msi
    cd "$app"
    cargo tauri bundle --bundles msi --ci --config "$cfg"
    set -- "$target"/release/bundle/msi/*.msi
    [ "$#" -eq 1 ] && [ -f "$1" ] || fail "expected one MSI in $target/release/bundle/msi, found: $*"
    cp "$1" "$out/"
    echo "build-msi: $out/$(basename "$1")"
}

[ "$#" -ge 1 ] || fail "usage: $0 stage|shell|bundle ..."
cmd=$1
shift
case "$cmd" in
stage) stage "$@" ;;
shell) shell "$@" ;;
bundle) bundle "$@" ;;
*) fail "unknown command $cmd" ;;
esac
