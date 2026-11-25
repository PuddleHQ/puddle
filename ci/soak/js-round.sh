#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# JavaScript round: a fresh install from the lockfile and a build.
set -eu
cd vite
rm -rf node_modules
CI=true pnpm install --frozen-lockfile --reporter=silent
pnpm run build >/dev/null
