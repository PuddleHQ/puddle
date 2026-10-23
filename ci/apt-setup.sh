#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run before the first `apt-get` of a CI job. The runner image sends apt to azure.archive.ubuntu.com,
# a DNS round robin over many backends; some of them serve 40 to 250 kB/s while the rest serve tens of
# MB/s, and a runner gets whichever its DNS answer names (a 62 MB install took 26 minutes). The
# stall timeout below cannot see a slow trickle, so the fix is the mirror: Canonical's own archive
# first (3 to 8 MB/s on every runner measured), Azure's mirror as the fallback for when it is down.
# APT_ETC points the script at another folder (the test); writes go through sudo only when needed.
set -euo pipefail

etc=${APT_ETC:-/etc/apt}
as_root=sudo
[ -w "$etc" ] && as_root=

# The image's sources name apt-mirrors.txt; if they ever stop doing so, the list below does nothing.
if ! grep -rqs 'mirror+file:/etc/apt/apt-mirrors.txt' "$etc/sources.list.d" "$etc/sources.list" 2>/dev/null &&
    [ -z "${APT_ETC:-}" ]; then
    echo "::warning::apt no longer reads apt-mirrors.txt; ci/apt-setup.sh has no effect"
fi

printf 'https://archive.ubuntu.com/ubuntu/\tpriority:1\nhttp://azure.archive.ubuntu.com/ubuntu/\tpriority:2\nhttps://security.ubuntu.com/ubuntu/\tpriority:3\n' |
    $as_root tee "$etc/apt-mirrors.txt" >/dev/null
$as_root mkdir -p "$etc/apt.conf.d"
# A download that goes silent for 20 s is dropped and retried instead of waited out.
printf 'Acquire::http::Timeout "20";\nAcquire::https::Timeout "20";\nAcquire::Retries "5";\n' |
    $as_root tee "$etc/apt.conf.d/99-ci-timeouts" >/dev/null
