#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Run before the first `apt-get` of a CI job. The runner image sends apt to azure.archive.ubuntu.com,
# a DNS round robin over many backends: most serve 5 to 100 MB/s, but some serve packages at 40 to
# 90 kB/s (index files at full speed), and a runner gets whichever backend its DNS answer names (a
# 62 MB install took 26 minutes). apt's stall timeout cannot see a slow trickle, so this script
# downloads 2 MB of a real package from the backend first:
#   fast: pin that backend (the answer is cached for the job, so apt reaches the one probed);
#   slow or failing: Canonical's own archive goes first (3 to 8 MB/s on every runner measured).
# Either way the other mirrors stay in the list as the fallback for an outage: Canonical's archive and
# mirrors.edge.kernel.org (a CDN run by kernel.org), so a day when both Ubuntu archives crawl still has a third.
# `--use azure|canonical|kernel` skips the probe and puts that mirror first (ci/apt-retry.sh rotates through
# them after a stall).
# With APT_DEBS_DIR set, downloaded .deb files go to that directory instead of /var/cache/apt/archives, so a
# job can restore it from and save it to the Actions cache (.github/actions/apt-debs-cache).
# Every step that can wait on the network is bounded and logged with its elapsed seconds, so a hang
# names its stage. Overrides for the test: APT_ETC, APT_HOSTS (hosts file), APT_PROBE_URL (skips the apt lookup); the
# script writes through sudo only when it cannot write itself.
set -euo pipefail

# It rewrites system files: refuse outside a CI runner (the test sets APT_ETC).
if [ "${GITHUB_ACTIONS:-}" != true ] && [ -z "${APT_ETC:-}" ]; then
    echo "ci/apt-setup.sh edits /etc and only runs on a CI runner; skipping"
    exit 0
fi

mode=${1:-probe}
use=
if [ "$mode" = --use ]; then
    use=${2:-}
    case "$use" in azure | canonical | kernel) ;; *) echo "usage: apt-setup.sh [--use azure|canonical|kernel]" >&2; exit 2 ;; esac
fi
etc=${APT_ETC:-/etc/apt}
hosts=${APT_HOSTS:-/etc/hosts}
host=azure.archive.ubuntu.com
probe=${APT_PROBE_URL:-}
as_root() { if [ -w "$etc" ] && [ -w "$hosts" ]; then "$@"; else sudo "$@"; fi; }

# A download that goes silent for 20 s is dropped and retried instead of waited out.
as_root mkdir -p "$etc/apt.conf.d"
conf='Acquire::http::Timeout "20";\nAcquire::https::Timeout "20";\nAcquire::Retries "5";\n'
if [ -n "${APT_DEBS_DIR:-}" ]; then
    # apt runs as root here, so the directory stays owned by the runner user (the cache action's) and no
    # sandbox user needs write access to it.
    as_root mkdir -p "$APT_DEBS_DIR/partial"
    conf="${conf}Dir::Cache::Archives \"$APT_DEBS_DIR\";\nAPT::Sandbox::User \"root\";\n"
fi
printf '%b' "$conf" | as_root tee "$etc/apt.conf.d/99-ci-timeouts" >/dev/null

# The image's sources name apt-mirrors.txt; if they ever stop doing so, the list below does nothing.
if [ -z "${APT_ETC:-}" ] &&
    ! grep -rqs 'mirror+file:/etc/apt/apt-mirrors.txt' /etc/apt/sources.list.d /etc/apt/sources.list; then
    echo "::warning::apt no longer reads apt-mirrors.txt; ci/apt-setup.sh has no effect"
fi

azure='http://azure.archive.ubuntu.com/ubuntu/'
canonical='https://archive.ubuntu.com/ubuntu/'
kernel='https://mirrors.edge.kernel.org/ubuntu/'
security='https://security.ubuntu.com/ubuntu/'

# 2 MB in 5 s is 0.4 MB/s: far above the slow backends and far below the fast ones. The package is the
# biggest one the jobs install; its index entry is fetched here, so the probe names a current file
# (apt prints the mirror list's name as the host, so the URL is rebuilt from the pool path).
stage() { echo "apt-setup: $1 (${SECONDS}s)"; }
if [ "$mode" = probe ] && [ -z "${APT_PROBE_URL:-}" ]; then
    as_root timeout -k 5 60 apt-get update -q >/dev/null 2>&1 || true
    stage "index refreshed"
    probe=$(timeout -k 5 30 apt-get download --print-uris libwebkit2gtk-4.1-0 2>/dev/null | awk -F"'" 'NR==1{print $2}' |
        sed -nE "s#^.*/(pool/.*)#http://$host/ubuntu/\\1#p" || true)
    stage "probe file named"
fi
ip=$(timeout -k 2 10 getent ahostsv4 "$host" 2>/dev/null | awk 'NR==1{print $1}' || true)
speed=0
if [ "$mode" = probe ] && [ -n "$probe" ]; then
    resolve=()
    [ -n "$ip" ] && [ -z "${APT_PROBE_URL:-}" ] && resolve=(--resolve "$host:80:$ip")
    # fetch-ok: a speed probe; a failure means "slow mirror", which is the answer.
    got=$(curl -s -o /dev/null --max-time 5 --range 0-2097151 "${resolve[@]}" \
        -w '%{size_download} %{speed_download}' "$probe" 2>/dev/null || true)
    # A server that ignores the range sends the whole file: that is as good as 2 MB.
    [ "${got%% *}" -ge 2097152 ] 2>/dev/null && speed=${got#* }
fi

if [ -n "$use" ]; then
    echo "apt mirror: a stall or failure was seen; ${!use} goes first"
    first=${!use}
elif [ "${speed%.*}" -gt 0 ] 2>/dev/null; then
    echo "apt mirror: $host ($ip) answered at ${speed%.*} B/s; keeping it first"
    if [ -n "$ip" ] && ! grep -q " $host\$" "$hosts"; then printf '%s %s\n' "$ip" "$host" | as_root tee -a "$hosts" >/dev/null; fi
    first=$azure
else
    echo "apt mirror: $host (${ip:-no address}) too slow or unreachable (${got:-no answer} from ${probe:-no package url}); archive.ubuntu.com goes first"
    first=$canonical
fi
# The rest follow in a fixed order after the first: Azure, Canonical, kernel.org.
list=("$first")
for m in "$azure" "$canonical" "$kernel"; do [ "$m" = "$first" ] || list+=("$m"); done
{
    n=1
    for m in "${list[@]}"; do printf '%s\tpriority:%s\n' "$m" "$n"; n=$((n + 1)); done
    printf '%s\tpriority:%s\n' "$security" "$n"
} | as_root tee "$etc/apt-mirrors.txt" >/dev/null
