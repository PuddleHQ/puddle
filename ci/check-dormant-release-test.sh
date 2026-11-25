#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/check-dormant-release.sh against small fake repositories.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
. "$(dirname "$0")/../scripts/test-fail.sh"

mkdir -p "$tmp/ci" "$tmp/.github/workflows"
# Test builds may use every feature; release builds without a feature list are fine.
cat >"$tmp/.github/workflows/ci.yml" <<'YML'
      - run: cargo build -p puddle-app --all-features --locked
      - run: cargo nextest run --workspace --all-features
YML
printf 'cargo tauri build --no-bundle --ci -- --locked\ncargo build --release --locked\n' >"$tmp/ci/build.sh"
"$here/check-dormant-release.sh" "$tmp" >"$tmp/out" 2>&1 || fail "a clean pipeline failed: $(cat "$tmp/out")"

printf 'cargo build --release --locked --features serve\n' >"$tmp/ci/build.sh"
if "$here/check-dormant-release.sh" "$tmp" 2>"$tmp/err"; then fail "--features serve was accepted"; fi
grep -q "ci/build.sh:1:" "$tmp/err" || fail "the line is not named: $(cat "$tmp/err")"

printf 'cargo tauri build --no-bundle \\\n  --features mise,embedded-ui \\\n  -- --locked\n' >"$tmp/ci/build.sh"
if "$here/check-dormant-release.sh" "$tmp" 2>"$tmp/err"; then fail "a continued --features mise was accepted"; fi
grep -q "ci/build.sh:1:" "$tmp/err" || fail "a continued command is not reported at its first line"

printf 'cargo build --release --all-features\n' >"$tmp/ci/build.sh"
if "$here/check-dormant-release.sh" "$tmp" 2>"$tmp/err"; then fail "--all-features on a release build was accepted"; fi

printf 'cargo build --release --features embedded-ui\n' >"$tmp/ci/build.sh"
"$here/check-dormant-release.sh" "$tmp" >/dev/null 2>&1 || fail "an unrelated feature was refused"

printf 'cargo build --release -F serve\n' >"$tmp/ci/build.sh"
if "$here/check-dormant-release.sh" "$tmp" 2>"$tmp/err"; then fail "-F serve was accepted"; fi

echo "check-dormant-release-test: ok"
