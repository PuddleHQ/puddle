#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# puddle's guest development certificate step: makes .NET's own tooling accept the certificate
# puddle put in /etc/puddle/dev-cert (the system store, Node's extra CAs and the bundle already
# have it). If the image has the .NET SDK and openssl, the certificate and key are imported as
# the user's ASP.NET development certificate and trusted for .NET's check, so Kestrel serves it
# and `dotnet dev-certs https --check --trust` passes.
#
# boot.sh runs this as a plan step at every boot. It never fails the boot over a tool: a .NET
# that refuses the certificate is reported on one line and the other tools keep working. It is
# idempotent: the import is skipped while the certificate is the one imported last time.
# POSIX sh. PUDDLE_ROOT relocates the paths for the unit tests only (boot.sh passes it on empty);
# PUDDLE_DEV_HOME is the home the .NET tooling uses (root's by default).
set -u

ROOT=${PUDDLE_ROOT:-}
DIR=$ROOT/etc/puddle/dev-cert
CRT=$DIR/localhost.crt
KEY=$DIR/localhost.key
STAMP=$DIR/.dotnet-imported

[ -f "$CRT" ] && [ -f "$KEY" ] || {
    echo "dev-cert: the certificate or its key is missing under /etc/puddle/dev-cert" >&2
    exit 1
}

if ! command -v dotnet >/dev/null 2>&1; then
    rm -f "$STAMP"
    echo "certificate in place; no .NET SDK in this image"
    exit 0
fi
if ! command -v openssl >/dev/null 2>&1; then
    rm -f "$STAMP"
    echo "certificate in place; .NET not set up (the image has no openssl to build the import file)"
    exit 0
fi

if command -v sha256sum >/dev/null 2>&1; then
    have=$(sha256sum "$CRT" | cut -d ' ' -f 1)
else
    have=$(cksum "$CRT" | cut -d ' ' -f 1,2 | tr ' ' -)
fi
if [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$have" ]; then
    echo "certificate unchanged; .NET already has it"
    exit 0
fi
rm -f "$STAMP"

home=${PUDDLE_DEV_HOME:-/root}
trust_dir=$home/.aspnet/dev-certs/trust
work=$(mktemp -d "${TMPDIR:-/tmp}/puddle-dev-cert.XXXXXX") || {
    echo "certificate in place; .NET not set up (no temporary folder)"
    exit 0
}
trap 'rm -rf "$work"' EXIT

# A one-off password for the import file, which is deleted before this script ends.
pw=$(head -c 24 /dev/urandom | od -An -tx1 | tr -d ' \n')
export PUDDLE_PFX_PW="$pw"
if ! openssl pkcs12 -export -inkey "$KEY" -in "$CRT" -passout env:PUDDLE_PFX_PW \
    -out "$work/dev.pfx" >"$work/openssl.log" 2>&1; then
    echo "certificate in place; .NET not set up (openssl could not build the import file: $(head -n 1 "$work/openssl.log"))"
    exit 0
fi

# .NET's trust check reads the hashed certificates under SSL_CERT_DIR (its own trust folder
# first), so it is set for these commands exactly as it is for the user's sessions.
export HOME="$home"
export SSL_CERT_DIR="$trust_dir:/etc/ssl/certs:/usr/lib/ssl/certs"
export DOTNET_CLI_TELEMETRY_OPTOUT=1 DOTNET_NOLOGO=1 DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
export DOTNET_GENERATE_ASPNET_CERTIFICATE=false

run() { # run <label> <command...>: quiet unless it fails
    label=$1
    shift
    if command -v timeout >/dev/null 2>&1; then
        timeout 120 "$@" >"$work/out.log" 2>&1
    else
        "$@" >"$work/out.log" 2>&1
    fi
    rc=$?
    if [ "$rc" -ne 0 ]; then
        echo "certificate in place; .NET refused it at '$label' (exit $rc): $(tr '\n' ' ' <"$work/out.log" | cut -c 1-300)"
        exit 0
    fi
}
run "dev-certs --clean --import" dotnet dev-certs https --clean --import "$work/dev.pfx" -p "$pw"
# `--trust` also tries the host side (it reaches for powershell.exe under WSL) and then exits
# non-zero though the guest's own trust is done: only the check below decides.
dotnet dev-certs https --trust >/dev/null 2>&1 || :
run "dev-certs --check --trust" dotnet dev-certs https --check --trust

printf '%s\n' "$have" >"$STAMP"
echo "certificate imported into .NET and trusted for its check"
