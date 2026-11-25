#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Agent round n: the simulated agent's turn. Three of the source's next commits go onto branch
# sim/soak-n, the tests run, then it asks 5 hosts the rules allow and one new made-up host
# (qa-soak-n.example.org), which lands in the Inbox and is denied by the next batch.
set -eu
n=$1
cd soak-click
git config user.name 'Soak Agent'
git config user.email soak-agent@puddle.invalid
git checkout -q -B "sim/soak-$n"
for c in $(git -C ../src-click rev-list --reverse HEAD | sed -n "$((3 * n + 2)),$((3 * n + 4))p"); do
  git -c core.hooksPath=/dev/null cherry-pick --allow-empty --keep-redundant-commits -X theirs "$c" >/dev/null 2>&1 \
    || { git cherry-pick --abort 2>/dev/null || true; }
done
../venv/bin/python -m pytest -q -x -p no:cacheprovider tests >/dev/null 2>&1 || true
for h in pypi.org files.pythonhosted.org github.com codeload.github.com registry.npmjs.org; do
  curl --retry 1 -s -o /dev/null --max-time 20 "https://$h/" || true
done
curl --retry 1 -s -o /dev/null --max-time 20 "https://qa-soak-$n.example.org/" || true
