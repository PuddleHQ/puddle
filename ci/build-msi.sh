#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Builds Puddle's per-user MSI on Windows (Git Bash), in two steps so a second MSI (the upgrade
# test's newer version) does not stage everything again.
#
#   ci/build-msi.sh stage <stage-dir>
#       Everything the installer carries besides the shell itself, in <stage-dir> (which must not
#       exist): runtime/ (the pinned msb and firmware from `cargo xtask runtime`, the guest agent,
#       the code-server release, licences and third-party notices) and bin/ (the `puddle` command
#       line). Needs the UI built (`cd ui && npm ci && npm run build`: the shell embeds it), `gh`
#       and `cargo-about` (see `cargo xtask`), network.
#   ci/build-msi.sh bundle <stage-dir> <out-dir> [<version>]
#       Builds the shell in release mode and has Tauri's WiX bundler pack it with the staged files
#       into <out-dir>/Puddle_<version>_x64_en-US.msi, from the template in
#       crates/puddle-app/wix/. Needs `cargo tauri` on PATH (ci/fetch-tauri-cli.sh). <version> is
#       numeric `major.minor.patch` and defaults to the workspace's.
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

bundle() {
    [ "$#" -ge 2 ] && [ "$#" -le 3 ] || fail "usage: $0 bundle <stage-dir> <out-dir> [<version>]"
    stage=$(cd "$1" && pwd)
    out=$2
    version=${3:-$(awk -F'"' '/^version = / { print $2; exit }' "$root/Cargo.toml")}
    case "$version" in
    [0-9]*.[0-9]*.[0-9]*) ;;
    *) fail "version '$version' is not numeric major.minor.patch" ;;
    esac
    [ -d "$stage/runtime" ] && [ -f "$stage/bin/puddle-$triple.exe" ] || fail "$stage is not a staged folder (run '$0 stage' first)"
    mkdir -p "$out"
    # Paths relative to tauri.conf.json: Tauri resolves a resource folder against its own pattern.
    rel=$(realpath --relative-to="$app" "$stage")
    overlay=$stage/tauri.msi.conf.json
    cat >"$overlay" <<JSON
{
  "version": "$version",
  "bundle": {
    "active": true,
    "resources": { "$rel/runtime": "runtime" },
    "externalBin": ["$rel/bin/puddle"]
  }
}
JSON
    # Tauri's output name has the version; a previous build of another version must not be taken for this one.
    target=$(target_dir)
    rm -f "$target"/release/bundle/msi/*.msi
    cd "$app"
    cargo tauri build --bundles msi --ci --config "$overlay" -- --locked
    set -- "$target"/release/bundle/msi/*.msi
    [ "$#" -eq 1 ] && [ -f "$1" ] || fail "expected one MSI in $target/release/bundle/msi, found: $*"
    cp "$1" "$out/"
    echo "build-msi: $out/$(basename "$1")"
}

[ "$#" -ge 1 ] || fail "usage: $0 stage|bundle ..."
cmd=$1
shift
case "$cmd" in
stage) stage "$@" ;;
bundle) bundle "$@" ;;
*) fail "unknown command $cmd" ;;
esac
