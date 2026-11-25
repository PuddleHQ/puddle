#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# The probe set the morning compares with hour 0: one host of each set the run switches on. The
# answer's status code is the result, never the time or the body.
set -u
for url in https://pypi.org/simple/ https://registry.npmjs.org/ https://github.com/; do
  code=$(curl --retry 1 -s -o /dev/null -w '%{http_code}' --max-time 20 "$url" || true)
  echo "$url $code"
done
