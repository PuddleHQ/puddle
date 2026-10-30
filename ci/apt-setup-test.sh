#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
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
# A server that ignores the range and sends the whole file counts as fast.
head -c 3000000 /dev/zero >"$tmp/whole"
run "a probe that sends more than asked" "file://$tmp/whole"
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$azure" ] || fail "a whole file: Azure is not first"

# --canonical skips the probe
printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_PROBE_URL="file://$tmp/full" "$here/apt-setup.sh" --canonical >"$tmp/out" ||
    fail "--canonical: the script failed"
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ] || fail "--canonical: Canonical is not first"

# apt-retry.sh: a command that finishes, one that stalls then works, one that always fails
export APT_ETC="$tmp/apt" APT_HOSTS="$tmp/hosts" APT_RETRY_NO_REPAIR=1 APT_FIRST_TRY_S=2 APT_RETRY_S=5
# The passing command gets a generous bound: with the 2 s of the stall cases below, a busy host that is
# slow to start even `true` would count as a stall and the run would be retried.
APT_FIRST_TRY_S=60 "$here/apt-retry.sh" true >"$tmp/out" 2>&1 || fail "retry: a passing command failed"
! grep -q retrying "$tmp/out" || fail "retry: a passing command was retried"
printf 'http://azure.archive.ubuntu.com/ubuntu/\tpriority:1\n' >"$tmp/apt/apt-mirrors.txt"
# The stubs tell the first try from the retry by the mirror list (Canonical first after the stop), not by a
# marker they write: a stub that starts after the bound is killed before it can write one.
cat >"$tmp/stall.sh" <<S
#!/usr/bin/env bash
if [ "\$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ]; then exit 0; fi
sleep 30
S
chmod +x "$tmp/stall.sh"
start=$SECONDS
"$here/apt-retry.sh" "$tmp/stall.sh" >"$tmp/out" 2>&1 || fail "retry: a stall was not recovered"
grep -q "status 124" "$tmp/out" || fail "retry: the warning does not name the timeout (status 124)"
[ $((SECONDS - start)) -lt 15 ] || fail "retry: the stalled try was not cut off"
[ "$(sed -n 1p "$tmp/apt/apt-mirrors.txt")" = "$canonical" ] || fail "retry: Canonical is not first after a stall"

# A command that fails both times: its own status comes back, with an error that names it
cat >"$tmp/fail7.sh" <<'S'
#!/usr/bin/env bash
exit 7
S
chmod +x "$tmp/fail7.sh"
rc=0
"$here/apt-retry.sh" "$tmp/fail7.sh" >"$tmp/out" 2>&1 || rc=$?
[ "$rc" = 7 ] || fail "retry: the second status was not passed on (got $rc)"
grep -q "^::error::apt-retry: '$tmp/fail7.sh' failed again (status 7) on archive.ubuntu.com after 5s$" "$tmp/out" ||
    fail "retry: no error after the second failure"
# The second try that times out names status 124
cat >"$tmp/hang.sh" <<'S'
#!/usr/bin/env bash
sleep 30
S
chmod +x "$tmp/hang.sh"
rc=0
"$here/apt-retry.sh" "$tmp/hang.sh" >"$tmp/out" 2>&1 || rc=$?
[ "$rc" = 124 ] || fail "retry: a second timeout did not give status 124 (got $rc)"
grep -q "failed again (status 124)" "$tmp/out" || fail "retry: no error after the second timeout"

# A failing repair step is a warning, not silence: apt-setup.sh gets a file where a directory belongs,
# and a fake sudo makes dpkg fail
: >"$tmp/notdir"
mkdir -p "$tmp/bin"
printf '#!/usr/bin/env bash\nexit 1\n' >"$tmp/bin/sudo"
chmod +x "$tmp/bin/sudo"
rc=0
PATH="$tmp/bin:$PATH" APT_ETC="$tmp/notdir" APT_RETRY_NO_REPAIR='' "$here/apt-retry.sh" "$tmp/fail7.sh" >"$tmp/out" 2>&1 || rc=$?
[ "$rc" = 7 ] || fail "retry: failing repair steps changed the status (got $rc)"
grep -q "^::warning::apt-retry: 'apt-setup.sh --canonical' failed" "$tmp/out" || fail "retry: no warning for apt-setup.sh"
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
APT_LOCKS="$tmp/lock-frontend" "$here/apt-retry.sh" "$tmp/lockhold.sh" >"$tmp/out" 2>&1 ||
    fail "retry: the retry failed on a lock the stopped try left behind: $(tail -3 "$tmp/out" | tr '\n' '|')"
grep -q "still holds an apt lock" "$tmp/out" || fail "retry: the lock holder was not named"
flock -n "$tmp/lock-frontend" true || fail "retry: the lock is still held"
echo "apt-setup-test: ok"
