#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: ci/apt-retry.sh <command that runs apt>...   (e.g. sudo apt-get install -y libfoo)
# ci/apt-setup.sh picks a mirror before the install, but a probe of 2 MB cannot see every slow
# backend: a backend that passed the probe still served the real install at 40 kB/s, and one runner
# hung before the probe printed anything. So the install itself is bounded: a normal one takes 10 to
# 60 s, and one that is still running after $APT_FIRST_TRY_S (default 180) is stopped, Canonical's
# archive is put first, dpkg is repaired and the command runs again (apt resumes partial downloads).
# Only the stall path is bounded this tightly; the retry gets $APT_RETRY_S (default 300).
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
first=${APT_FIRST_TRY_S:-180}
second=${APT_RETRY_S:-300}
[ "$#" -gt 0 ] || { echo "usage: apt-retry.sh <command>..." >&2; exit 2; }

if timeout -k 10 "$first" "$@"; then exit 0; fi
status=$?
echo "::warning::apt-retry: '$*' ended with status $status after at most ${first}s; retrying on archive.ubuntu.com"
"$here/apt-setup.sh" --canonical || true
[ -n "${APT_RETRY_NO_REPAIR:-}" ] || sudo dpkg --configure -a || true
exec timeout -k 10 "$second" "$@"
