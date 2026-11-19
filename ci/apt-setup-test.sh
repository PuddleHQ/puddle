#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/apt-setup.sh against a scratch /etc/apt and a probe file instead of the network.
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
holder=
trap 'rm -rf "$tmp"; [ -z "$holder" ] || kill "$holder" 2>/dev/null || true' EXIT
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
    [ "$(wc -l <"$tmp/apt/apt-mirrors.txt")" = 4 ] || fail "$1: the list is not exactly four mirrors"
    grep -q "^https://mirrors.edge.kernel.org/ubuntu/	priority:[23]$" "$tmp/apt/apt-mirrors.txt" || fail "$1: no kernel.org fallback"
    grep -q "^https://security.ubuntu.com/ubuntu/	priority:4$" "$tmp/apt/apt-mirrors.txt" || fail "$1: no security mirror"
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
# A server that ignores the range and sends the whole file counts as fast.
head -c 3000000 /dev/zero >"$tmp/whole"
run "a probe that sends more than asked" "file://$tmp/whole"
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$azure" ] || fail "a whole file: Azure is not first"

kernel="https://mirrors.edge.kernel.org/ubuntu/	priority:1"
# --use skips the probe and puts the named mirror first, the others behind it
for pair in "canonical:$canonical" "kernel:$kernel" "azure:$azure"; do
    printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
    APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_PROBE_URL="file://$tmp/full" "$here/apt-setup.sh" --use "${pair%%:*}" >"$tmp/out" ||
        fail "--use ${pair%%:*}: the script failed"
    [ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "${pair#*:}" ] || fail "--use ${pair%%:*}: that mirror is not first"
    [ "$(wc -l <"$tmp/apt/apt-mirrors.txt")" = 4 ] || fail "--use ${pair%%:*}: the list is not four mirrors"
done
APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" "$here/apt-setup.sh" --use bogus >/dev/null 2>&1 && fail "--use bogus was accepted"

# APT_DEBS_DIR moves apt's download directory (for the Actions cache) and drops the sandbox user
APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_PROBE_URL="file://$tmp/full" APT_DEBS_DIR="$tmp/debs" "$here/apt-setup.sh" >"$tmp/out" ||
    fail "debs dir: the script failed"
grep -q "Dir::Cache::Archives \"$tmp/debs\";" "$tmp/apt/apt.conf.d/99-ci-timeouts" || fail "debs dir: archives not redirected"
grep -q 'APT::Sandbox::User "root";' "$tmp/apt/apt.conf.d/99-ci-timeouts" || fail "debs dir: sandbox user kept"
[ -d "$tmp/debs/partial" ] || fail "debs dir: no partial directory"
APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_PROBE_URL="file://$tmp/full" "$here/apt-setup.sh" >"$tmp/out" || fail "no debs dir: the script failed"
! grep -q "Dir::Cache" "$tmp/apt/apt.conf.d/99-ci-timeouts" || fail "no debs dir: archives redirected anyway"

# apt-retry.sh: a command that finishes, one that stalls then works, one that always fails
# Time limits depend on what a case proves. A case that must NOT time out gets bounds far above any spawn time
# (a host under load can take many seconds to start bash); a case that needs a timeout uses a stub that runs
# far longer than its bound, so the host cannot change the outcome. The tries that must succeed get a long bound.
export APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_RETRY_NO_REPAIR=1 APT_FIRST_TRY_S=60 APT_RETRY_S=60
# The stall cases hold no lock; the real dpkg locks would make apt-retry scan /proc for holders (slow on a busy
# host, which broke the 15 s bound below) and stop a real apt-get that happens to run there.
export APT_LOCKS="$tmp/no-such-lock"
# The passing command gets a generous bound: a busy host that is slow to start even `true` must not count as a
# stall and get the run retried.
"$here/apt-retry.sh" true >"$tmp/out" 2>&1 || fail "retry: a passing command failed"
! grep -q retrying "$tmp/out" || fail "retry: a passing command was retried"
printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
# The stubs tell the first try from the retry by the mirror list (Canonical first after the stop), not by a
# marker they write: a stub that starts after the bound is killed before it can write one.
cat >"$tmp/stall.sh" <<S
#!/usr/bin/env bash
if [ "\$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ]; then exit 0; fi
sleep 120
S
chmod +x "$tmp/stall.sh"
APT_FIRST_TRY_S=2 "$here/apt-retry.sh" "$tmp/stall.sh" >"$tmp/out" 2>&1 || fail "retry: a stall was not recovered"
grep -q "status 124" "$tmp/out" || fail "retry: the warning does not name the timeout (status 124)"
# Status 124 above proves the try was cut off: an uncut one would end after the stub's 120 s with status 0 and no
# warning. No wall-clock bound here, so the host speed cannot change the result.
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ] || fail "retry: Canonical is not first after a stall"

# A command that fails both times: its own status comes back, with an error that names it
cat >"$tmp/fail7.sh" <<'S'
#!/usr/bin/env bash
exit 7
S
chmod +x "$tmp/fail7.sh"
rc=0
printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
"$here/apt-retry.sh" "$tmp/fail7.sh" >"$tmp/out" 2>&1 || rc=$?
[ "$rc" = 7 ] || fail "retry: the second status was not passed on (got $rc)"
grep -q "^::error::apt-retry: '$tmp/fail7.sh' failed again (status 7) on mirrors.edge.kernel.org after 60s; mirrors tried: azure.archive.ubuntu.com, archive.ubuntu.com, mirrors.edge.kernel.org$" "$tmp/out" ||
    fail "retry: no error naming every mirror after the last failure: $(tail -2 "$tmp/out" | tr '\n' '|')"
# The second try that times out names status 124
cat >"$tmp/hang.sh" <<'S'
#!/usr/bin/env bash
sleep 30
S
chmod +x "$tmp/hang.sh"
rc=0
APT_FIRST_TRY_S=2 APT_RETRY_S=5 "$here/apt-retry.sh" "$tmp/hang.sh" >"$tmp/out" 2>&1 || rc=$?
[ "$rc" = 124 ] || fail "retry: a second timeout did not give status 124 (got $rc)"
grep -q "failed again (status 124)" "$tmp/out" || fail "retry: no error after the last timeout"

# Both Ubuntu archives stall: the third mirror (kernel.org) is reached, and only after the two others were tried.
# The fake apt-get serves only from kernel.org (read from the list apt-setup.sh wrote), as a download would.
cat >"$tmp/apt3.sh" <<S
#!/usr/bin/env bash
sed -n 1p "$tmp/apt/apt-mirrors.txt" >>"$tmp/apt3.log"
if [ "\$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$kernel" ]; then exit 0; fi
sleep 120
S
chmod +x "$tmp/apt3.sh"
printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
: >"$tmp/apt3.log"
APT_FIRST_TRY_S=2 APT_RETRY_S=2 "$here/apt-retry.sh" "$tmp/apt3.sh" >"$tmp/out" 2>&1 ||
    fail "retry: the third mirror was not reached: $(tail -3 "$tmp/out" | tr '\n' '|')"
[ "$(wc -l <"$tmp/apt3.log")" = 3 ] || fail "retry: not exactly three tries"
sed -n 2p "$tmp/apt3.log" | grep -q archive.ubuntu.com || fail "retry: the second try was not on Canonical"
grep -q "retrying on the kernel mirror" "$tmp/out" || fail "retry: no warning for the kernel step"
grep -q "ended with status 124 on archive.ubuntu.com" "$tmp/out" || fail "retry: the stalled mirror is not named"
# A warm cache needs no mirror at all: the fake apt-get reads the archives directory from the conf apt-setup.sh wrote
# and finds the .deb there, so it succeeds although every mirror would hang.
mkdir -p "$tmp/warm/partial"
: >"$tmp/warm/libfoo.deb"
APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_PROBE_URL="file://$tmp/missing" APT_DEBS_DIR="$tmp/warm" "$here/apt-setup.sh" >"$tmp/out" || fail "warm: setup failed"
cat >"$tmp/aptwarm.sh" <<S
#!/usr/bin/env bash
dir=\$(sed -n 's/^Dir::Cache::Archives "\(.*\)";\$/\1/p' "$tmp/apt/apt.conf.d/99-ci-timeouts")
[ -e "\$dir/libfoo.deb" ] && exit 0
sleep 120
S
chmod +x "$tmp/aptwarm.sh"
"$here/apt-retry.sh" "$tmp/aptwarm.sh" >"$tmp/out" 2>&1 || fail "warm: a warm cache still needed the network"
! grep -q retrying "$tmp/out" || fail "warm: a warm install was retried"
mv "$tmp/warm/libfoo.deb" "$tmp/libfoo.moved"
rc=0
APT_FIRST_TRY_S=1 APT_RETRY_S=1 "$here/apt-retry.sh" "$tmp/aptwarm.sh" >"$tmp/out" 2>&1 || rc=$?
[ "$rc" = 124 ] || fail "cold: an empty cache did not take the network path (got $rc)"

# A failing repair step is a warning, not silence: apt-setup.sh gets a file where a directory belongs,
# and a fake sudo makes dpkg fail
: >"$tmp/notdir"
mkdir -p "$tmp/bin"
printf '#!/usr/bin/env bash\nexit 1\n' >"$tmp/bin/sudo"
chmod +x "$tmp/bin/sudo"
rc=0
PATH="$tmp/bin:$PATH" APT_ETC="$tmp/notdir" APT_RETRY_NO_REPAIR='' "$here/apt-retry.sh" "$tmp/fail7.sh" >"$tmp/out" 2>&1 || rc=$?
[ "$rc" = 7 ] || fail "retry: failing repair steps changed the status (got $rc)"
grep -q "^::warning::apt-retry: 'apt-setup.sh --use canonical' failed" "$tmp/out" || fail "retry: no warning for apt-setup.sh"
grep -q "^::warning::apt-retry: 'dpkg --configure -a' failed" "$tmp/out" || fail "retry: no warning for dpkg"
# A try that times out must not leave an apt lock held: this test starts a holder in its own session
# (as sudo does) and confirms it holds the lock before the first try, so a slow stub cannot skip the
# case. The retry fails while the holder runs.
cat >"$tmp/lockhold.sh" <<S
#!/usr/bin/env bash
lock="$tmp/lock-frontend"
if [ "\$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ]; then
    flock -n "\$lock" true || { echo "E: Could not get lock \$lock" >&2; exit 1; }
    exit 0
fi
sleep 60
S
chmod +x "$tmp/lockhold.sh"
printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
: >"$tmp/lock-frontend"
setsid flock "$tmp/lock-frontend" sleep 60 &
holder=$!
for _ in $(seq 200); do flock -n "$tmp/lock-frontend" true || break; sleep 0.05; done
flock -n "$tmp/lock-frontend" true && fail "retry: the lock holder did not start"
APT_FIRST_TRY_S=2 APT_LOCKS="$tmp/lock-frontend" "$here/apt-retry.sh" "$tmp/lockhold.sh" >"$tmp/out" 2>&1 ||
    fail "retry: the retry failed on a lock the stopped try left behind: $(tail -3 "$tmp/out" | tr '\n' '|')"
grep -q "still holds an apt lock" "$tmp/out" || fail "retry: the lock holder was not named"
flock -n "$tmp/lock-frontend" true || fail "retry: the lock is still held"
echo "apt-setup-test: ok"
