#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Checks what Puddle's Azure CLI source assumes about `az account get-access-token` against the
# pinned azure-cli release in ci/az-cli.pin, with no Azure account:
#   1. the two source files at the release's tag still hash to the pin (a changed file means: read it again);
#   2. they still build the answer from the fields Puddle reads (accessToken, expires_on, expiresOn, tenant,
#      tokenType), the tenant is echoed as asked, and the no-login message is "Please run 'az login' to setup account.";
#   3. the real `az` of that version, run with an empty config dir and the exact arguments the source uses, exits non-zero
#      with that message on stderr and nothing on stdout (the not-signed-in error).
# AZ (the program, default `az`), AZ_PIN_FILE and AZ_SOURCE_URL replace the real ones in tests; step 3 also checks
# `az version` against the pin. Run nightly by live-contracts.yml, never on a pull request: it needs the network.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# shellcheck source=ci/fetch-retry.sh
. "$here/fetch-retry.sh"
pins=${AZ_PIN_FILE:-$here/az-cli.pin}
az=${AZ:-az}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() { echo "check-az-contract: $1" >&2; exit 1; }

version=$(awk '$1 == "version" { print $2; exit }' "$pins")
[ -n "$version" ] || fail "$pins has no version line"
base=${AZ_SOURCE_URL:-https://raw.githubusercontent.com/Azure/azure-cli/azure-cli-$version/src}

n=0
while read -r want path; do
    case $want in '' | '#'* | version) continue ;; esac
    out="$work/$(basename "$path")"
    download check-az-contract "$base/$path" "$out" 120
    got=$(sha256sum "$out" | cut -d' ' -f1)
    [ "$got" = "$want" ] || fail "$path at azure-cli $version hashes to $got, the pin has $want: read it again (ci/az-cli.pin)"
    n=$((n + 1))
done <"$pins"
[ "$n" -eq 2 ] || fail "$pins must pin exactly custom.py and _profile.py (found $n)"

custom="$work/custom.py"
profile="$work/_profile.py"
for key in tokenType accessToken expires_on expiresOn tenant; do
    grep -q "'$key':" "$custom" || fail "custom.py no longer builds the \`$key\` field"
done
grep -q "str(tenant if tenant else account\[_TENANT_ID\])" "$profile" ||
    fail "_profile.py no longer returns the asked tenant as typed (a domain name is not resolved)"
grep -q "^_AZ_LOGIN_MESSAGE = \"Please run 'az login' to setup account.\"" "$profile" ||
    fail "_profile.py no longer words the not-signed-in error"

have=$("$az" version --query '"azure-cli"' -o tsv 2>/dev/null) || fail "\`$az version\` failed"
[ "$have" = "$version" ] || fail "the az on PATH is $have, the pin is $version"

mkdir "$work/cfg"
rc=0
AZURE_CONFIG_DIR="$work/cfg" AZURE_CORE_NO_COLOR=1 AZURE_CORE_COLLECT_TELEMETRY=false \
    "$az" account get-access-token --resource 499b84ac-1321-427f-aa17-267ca6975798 \
    --tenant contoso.example --output json >"$work/stdout" 2>"$work/stderr" || rc=$?
[ "$rc" -ne 0 ] || fail "az answered without a sign-in (exit 0)"
[ ! -s "$work/stdout" ] || fail "az printed to stdout without a sign-in"
grep -q "Please run 'az login' to setup account." "$work/stderr" ||
    fail "the not-signed-in stderr changed: $(tr '\n' ' ' <"$work/stderr" | cut -c1-300)"
echo "check-az-contract: azure-cli $version source and not-signed-in error match (exit $rc)"
