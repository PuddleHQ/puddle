#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# JavaScript round: a fresh install from the lockfile and a build.
# Quiet on success; on failure pnpm's own output goes to stderr so the soak log
# shows why (the silent reporter hid a broken install).
set -eu
# vite's playwright-chromium postinstall downloads a browser from the Playwright CDN, which the
# guest's egress does not reach (ERR_SOCKET_CLOSED). The round installs and builds; vite's browser
# tests are not part of it, so the download is skipped.
export PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD=1
cd vite
rm -rf node_modules
log=$(mktemp)
trap 'rm -f "$log"' EXIT
if ! CI=true pnpm install --frozen-lockfile >"$log" 2>&1; then
  cat "$log" >&2
  echo "js-round: pnpm install failed" >&2
  exit 1
fi
if ! pnpm run build >"$log" 2>&1; then
  cat "$log" >&2
  echo "js-round: pnpm run build failed" >&2
  exit 1
fi
