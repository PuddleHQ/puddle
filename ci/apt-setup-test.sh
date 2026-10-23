#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests ci/apt-setup.sh against a scratch /etc/apt and a probe file instead of the network.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "apt-setup-test: FAIL: $1" >&2; exit 1; }

mkdir -p "$tmp/apt"
head -c 2097152 /dev/zero >"$tmp/full"
head -c 1000000 /dev/zero >"$tmp/cut"
: >"$tmp/hosts"

# run <name> <probe url>: runs the script twice (it must give the same list the second time)
run() {
    for _ in 1 2; do
        printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
        APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_PROBE_URL="$2" "$here/apt-setup.sh" >"$tmp/out" ||
            fail "$1: the script failed"
    done
    [ "$(wc -l <"$tmp/apt/apt-mirrors.txt")" = 3 ] || fail "$1: the list is not exactly three mirrors"
    grep -q "^https://security.ubuntu.com/ubuntu/	priority:3$" "$tmp/apt/apt-mirrors.txt" || fail "$1: no security mirror"
    conf=$tmp/apt/apt.conf.d/99-ci-timeouts
    grep -q 'Acquire::http::Timeout "20"' "$conf" || fail "$1: no http timeout"
    grep -q 'Acquire::Retries "5"' "$conf" || fail "$1: no retries"
}

azure="http://azure.archive.ubuntu.com/ubuntu/	priority:1"
canonical="https://archive.ubuntu.com/ubuntu/	priority:1"

run "a fast mirror" "file://$tmp/full"
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$azure" ] || fail "a fast mirror: Azure is not first"
[ "$(grep -c azure.archive.ubuntu.com "$tmp/hosts")" -le 1 ] || fail "a fast mirror: the hosts pin was written twice"
grep -q "keeping it first" "$tmp/out" || fail "a fast mirror: no message"

run "a probe that returns too little" "file://$tmp/cut"
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ] || fail "a short answer: Canonical is not first"
grep -q "^http://azure.archive.ubuntu.com/ubuntu/	priority:2$" "$tmp/apt/apt-mirrors.txt" || fail "a short answer: no Azure fallback"

run "a probe that fails" "file://$tmp/missing"
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ] || fail "a failed probe: Canonical is not first"
echo "apt-setup-test: ok"
