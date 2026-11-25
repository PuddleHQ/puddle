#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# qa-soak-agent: the history source (a clone of click) and the repository the rounds commit to.
set -eu
sudo apt-get update -q
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends git curl python3 python3-venv ca-certificates
[ -d src-click ] || git clone -q https://github.com/pallets/click src-click
if [ ! -d soak-click ]; then
  git clone -q src-click soak-click
  git -C soak-click reset -q --hard "$(git -C src-click rev-list --max-parents=0 HEAD | tail -1)"
fi
[ -d venv ] || python3 -m venv venv
./venv/bin/pip install -q pytest
