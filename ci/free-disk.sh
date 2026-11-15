#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Frees space on a hosted Linux runner that starts with little disk. Some runners of the pool start with a 72 GB
# root disk of which the image already fills 59 GB (14 GB free), where a normal one has 145 GB (86 GB free); the
# VM test job writes about 13 GB and the nextest archive extraction alone filled the rest. Run as the first step
# of a job. When the root disk has at least FREE_DISK_BELOW_GB (default 60) free it does nothing. Otherwise it
# removes preinstalled toolchains that no job here uses (the list below; the workflows and ci/ never call them:
# Rust comes from rustup in the home folder, Node from setup-node, the browsers from Playwright) and prints how
# much each one freed, to stdout and to the job summary. Only the five big ones are listed: measured on image
# 20261101.360.1 they free 24 GB of the 33 GB the whole set did (android 11.3, dotnet 5.9, ghcup 3.7, swift 3.5,
# CodeQL 1.4 GB), and deleting is I/O bound (the whole set took 170 s), so the small ones are not worth the time.
#
#   ci/free-disk.sh        the real run
#   FREE_DISK_PATHS=... FREE_DISK_SUDO= FREE_DISK_ROOT=<dir> ci/free-disk.sh   (ci/free-disk-test.sh)
#
# Evidence plus a remedy, never a gate: it exits 0 whatever it manages to remove.
set -uo pipefail

below=${FREE_DISK_BELOW_GB:-60}
root=${FREE_DISK_ROOT:-/}
sudo_cmd=${FREE_DISK_SUDO-sudo}
# shellcheck disable=SC2206 # why: a space-separated list of paths without spaces
paths=(${FREE_DISK_PATHS:-/usr/local/lib/android /usr/share/dotnet /usr/local/.ghcup /usr/share/swift /opt/hostedtoolcache/CodeQL})

free_gb() { df -Pk "$root" | awk 'NR == 2 { print int($4 / 1048576) }'; }
# why: du -sm rounds up, so the directory blocks of a tree that holds exactly 3 MiB (4 KiB each on ext4) read as 4;
# KiB rounded to the nearest MiB does not depend on the file system's block size
size_mb() { $sudo_cmd du -sk "$1" 2>/dev/null | awk '{ print int(($1 + 512) / 1024) }'; }

start=$(free_gb)
if [ "$start" -ge "$below" ]; then
    echo "free-disk: ${start} GB free, at least ${below}: nothing to do"
    exit 0
fi

out=$(mktemp)
echo "free-disk: ${start} GB free, under ${below}: removing preinstalled toolchains the jobs never use"
t0=$(date +%s)
pids=()
for p in "${paths[@]}"; do
    [ -e "$p" ] || continue
    (
        mb=$(size_mb "$p")
        $sudo_cmd rm -rf -- "$p" 2>/dev/null
        printf '%s\t%s\n' "${mb:-0}" "$p" >>"$out"
    ) &
    pids+=($!)
done
for pid in "${pids[@]}"; do wait "$pid"; done
if command -v docker >/dev/null 2>&1; then
    mb=$($sudo_cmd docker system df --format '{{.Reclaimable}}' 2>/dev/null | head -1 || true)
    $sudo_cmd docker system prune -af >/dev/null 2>&1 || true
    printf '0\tdocker images and containers (reclaimable before: %s)\n' "${mb:-unknown}" >>"$out"
fi
$sudo_cmd apt-get clean >/dev/null 2>&1 || true

end=$(free_gb)
{
    echo "### free disk: ${start} GB free before, ${end} GB after ($(($(date +%s) - t0)) s)"
    echo '```'
    sort -rn "$out" | awk -F'\t' '{ printf "%7d MiB  %s\n", $1, $2 }'
    df -h "$root" /mnt 2>/dev/null
    echo '```'
} | tee -a "${GITHUB_STEP_SUMMARY:-/dev/null}"
rm -f -- "$out"
if [ "$end" -lt "$below" ]; then
    echo "::warning title=runner still low on disk::${end} GB free after removing the unused toolchains (floor ${below} GB)"
fi
exit 0
