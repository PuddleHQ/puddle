#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Tests ci/check-az-contract.sh against source files served as files and a fake `az` (no network).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
. "$(dirname "$0")/../scripts/test-fail.sh"

src="$tmp/src"
mkdir -p "$src/azure-cli/azure/cli/command_modules/profile" "$src/azure-cli-core/azure/cli/core"
c="$src/azure-cli/azure/cli/command_modules/profile/custom.py"
p="$src/azure-cli-core/azure/cli/core/_profile.py"
printf "    'tokenType': a,\n    'accessToken': b,\n    'expires_on': c,\n    'expiresOn': d,\n    'tenant': tenant\n" >"$c"
printf "_AZ_LOGIN_MESSAGE = \"Please run 'az login' to setup account.\"\nx = str(tenant if tenant else account[_TENANT_ID])\n" >"$p"
pin() {
    printf 'version 9.9.9\n%s azure-cli/azure/cli/command_modules/profile/custom.py\n%s azure-cli-core/azure/cli/core/_profile.py\n' \
        "$(sha256sum "$c" | cut -d' ' -f1)" "$(sha256sum "$p" | cut -d' ' -f1)" >"$tmp/pins"
}
pin
cat >"$tmp/az" <<'F'
#!/bin/sh
case $1 in
    version) echo "${FAKE_AZ_VERSION:-9.9.9}" ;;
    account) [ -n "${FAKE_AZ_OK:-}" ] && { echo '{}'; exit 0; }; echo "ERROR: ${FAKE_AZ_ERR:-Please run 'az login' to setup account.}" >&2; exit 1 ;;
esac
F
chmod +x "$tmp/az"
export AZ="$tmp/az" AZ_PIN_FILE="$tmp/pins" AZ_SOURCE_URL="file://$src" FETCH_ATTEMPTS=1
run() { "$here/check-az-contract.sh" >"$tmp/out" 2>"$tmp/err"; }

run || fail "a matching source and error failed: $(cat "$tmp/err")"
grep -q "azure-cli 9.9.9" "$tmp/out" || fail "success line missing"

FAKE_AZ_ERR="Something else" run && fail "a changed not-signed-in text was accepted"
grep -q "stderr changed" "$tmp/err" || fail "the changed stderr is not named"
FAKE_AZ_OK=1 run && fail "an answer without a sign-in was accepted"
FAKE_AZ_VERSION=1.0.0 run && fail "a different az version was accepted"
grep -q "pin is 9.9.9" "$tmp/err" || fail "the version mismatch is not named"

printf "    'tokenType': a,\n" >>"$c"
run && fail "a changed source file was accepted"
grep -q "hashes to" "$tmp/err" || fail "the hash mismatch is not named"
printf "    'tokenType': a,\n    'accessToken': b,\n    'expires_on': c,\n    'expiresOn': d,\n" >"$c"
pin
run && fail "a source without the tenant field was accepted"
grep -q "builds the .tenant. field" "$tmp/err" || fail "the missing field is not named"
echo "check-az-contract-test: ok"
