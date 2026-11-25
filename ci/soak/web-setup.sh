#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# qa-soak-web: express's hello-world example on port 3000. Run again after every start: it
# starts the server only when nothing listens there.
set -eu
if ! command -v node >/dev/null 2>&1; then
  sudo apt-get update -q
  sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends nodejs npm git ca-certificates
fi
if [ ! -d express ]; then
  git clone -q --depth 1 https://github.com/expressjs/express
  (cd express && npm install -q --no-audit --no-fund)
fi
if ! curl --retry 1 -s -o /dev/null --max-time 5 http://127.0.0.1:3000/; then
  cd express
  PORT=3000 nohup node examples/hello-world/index.js > "$HOME/w4.log" 2>&1 &
  i=0
  until curl --retry 1 -s -o /dev/null --max-time 2 http://127.0.0.1:3000/; do
    i=$((i + 1)); [ "$i" -lt 60 ] || { cat "$HOME/w4.log"; exit 1; }
    sleep 1
  done
fi
