#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Run before the first `apt-get` of a CI job. The runner image sends apt to azure.archive.ubuntu.com,
# a DNS round robin over many backends: most serve 5 to 100 MB/s, but some serve 40 to 250 kB/s, and a
# runner gets whichever backend its DNS answer names (a 62 MB install took 26 minutes). apt's stall
# timeout cannot see a slow trickle, so this script downloads 3 MB from the backend first:
#   fast: pin that backend (the answer is cached for the job, so apt reaches the one probed);
#   slow or failing: Canonical's own archive goes first (3 to 8 MB/s on every runner measured).
# Either way the other mirror stays in the list as the fallback for an outage.
# Overrides for the test: APT_ETC, APT_HOSTS (hosts file), APT_PROBE_URL; the script writes through sudo
# only when it cannot write itself.
set -euo pipefail

etc=${APT_ETC:-/etc/apt}
hosts=${APT_HOSTS:-/etc/hosts}
host=azure.archive.ubuntu.com
probe=${APT_PROBE_URL:-http://$host/ubuntu/dists/noble/universe/binary-amd64/Packages.xz}
as_root() { if [ -w "$etc" ] && [ -w "$hosts" ]; then "$@"; else sudo "$@"; fi; }

# The image's sources name apt-mirrors.txt; if they ever stop doing so, the list below does nothing.
if [ -z "${APT_ETC:-}" ] &&
    ! grep -rqs 'mirror+file:/etc/apt/apt-mirrors.txt' /etc/apt/sources.list.d /etc/apt/sources.list; then
    echo "::warning::apt no longer reads apt-mirrors.txt; ci/apt-setup.sh has no effect"
fi

azure='http://azure.archive.ubuntu.com/ubuntu/'
canonical='https://archive.ubuntu.com/ubuntu/'
security='https://security.ubuntu.com/ubuntu/'

# 3 MB in 5 s is 0.6 MB/s: far above the slow backends and far below the fast ones.
ip=$(getent ahostsv4 "$host" 2>/dev/null | awk 'NR==1{print $1}' || true)
speed=0
if [ -n "$ip" ] || [ -n "${APT_PROBE_URL:-}" ]; then
    resolve=()
    [ -n "$ip" ] && [ -z "${APT_PROBE_URL:-}" ] && resolve=(--resolve "$host:80:$ip")
    got=$(curl -s -o /dev/null --max-time 5 --range 0-3145727 "${resolve[@]}" \
        -w '%{size_download} %{speed_download}' "$probe" 2>/dev/null || true)
    [ "${got%% *}" = 3145728 ] && speed=${got#* }
fi

if [ "${speed%.*}" -gt 0 ] 2>/dev/null; then
    echo "apt mirror: $host ($ip) answered at ${speed%.*} B/s; keeping it first"
    if [ -n "$ip" ]; then printf '%s %s\n' "$ip" "$host" | as_root tee -a "$hosts" >/dev/null; fi
    first=$azure second=$canonical
else
    echo "apt mirror: $host (${ip:-no address}) too slow or unreachable (${got:-no answer}); archive.ubuntu.com goes first"
    first=$canonical second=$azure
fi
printf '%s\tpriority:1\n%s\tpriority:2\n%s\tpriority:3\n' "$first" "$second" "$security" |
    as_root tee "$etc/apt-mirrors.txt" >/dev/null
as_root mkdir -p "$etc/apt.conf.d"
# A download that goes silent for 20 s is dropped and retried instead of waited out.
printf 'Acquire::http::Timeout "20";\nAcquire::https::Timeout "20";\nAcquire::Retries "5";\n' |
    as_root tee "$etc/apt.conf.d/99-ci-timeouts" >/dev/null
