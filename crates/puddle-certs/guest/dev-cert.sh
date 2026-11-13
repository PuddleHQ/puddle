#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Puddle's guest development certificate step: makes .NET's own tooling accept the certificate
# Puddle put in /etc/puddle/dev-cert (the system store, Node's extra CAs and the bundle already
# have it). If the image has the .NET SDK and openssl, the certificate and key are imported as
# the user's ASP.NET development certificate and trusted for .NET's check, so Kestrel serves it
# and `dotnet dev-certs https --check --trust` passes.
#
# boot.sh runs this as a plan step at every boot. It never fails the boot over a tool: a .NET
# that refuses the certificate is reported on one line and the other tools keep working. Each
# .NET command gets 12 s (a boot has 60 s in all; the commands take about a second). It is
# idempotent: the import is skipped while the certificate is the one imported last time into the
# same home.
# POSIX sh. PUDDLE_ROOT relocates the paths for the unit tests only (boot.sh passes it on empty).
# boot.sh sets PUDDLE_SESSION_HOME and PUDDLE_SESSION_OWNER (<uid>:<gid>) to the session user's:
# .NET keeps the certificate in that home, so the import goes there and its files are given to the
# user, as Kestrel run by that user reads them (root's home and owner when not set).
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
home=${PUDDLE_SESSION_HOME:-/root}
owner=${PUDDLE_SESSION_OWNER:-0:0}
have="$have $home"
if [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$have" ]; then
    echo "certificate unchanged; .NET already has it"
    exit 0
fi
rm -f "$STAMP"

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
        timeout 12 "$@" >"$work/out.log" 2>&1
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
if command -v timeout >/dev/null 2>&1; then
    timeout 12 dotnet dev-certs https --trust >/dev/null 2>&1 || :
else
    dotnet dev-certs https --trust >/dev/null 2>&1 || :
fi
run "dev-certs --check --trust" dotnet dev-certs https --check --trust

# What .NET wrote below the home as root belongs to the session user. Under the unit tests' fake
# root (not root) the request is recorded in $PUDDLE_ROOT/owned instead.
if [ "$owner" != 0:0 ]; then
    for d in "$home/.dotnet" "$home/.aspnet"; do
        [ -e "$ROOT$d" ] || continue
        if [ -n "$ROOT" ]; then
            printf '%s %s\n' "$owner" "$d" >>"$ROOT/owned"
        elif ! chown -R -h "$owner" "$d"; then
            echo "certificate in place; .NET set up, but $d could not be given to the session user"
            exit 0
        fi
    done
fi

printf '%s\n' "$have" >"$STAMP"
echo "certificate imported into .NET and trusted for its check"
