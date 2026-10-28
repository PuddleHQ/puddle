#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Java's own trust store: imports Puddle's extra CAs (the host's corporate and user-added roots,
# Puddle's own CAs; /etc/puddle/extra-cas.pem) into the `cacerts` of every JDK or JRE the image
# has, so a Java client trusts the connections Puddle decrypts. Java reads no certificate
# variable, so an import is the one way that works for every launcher (java, mvn, gradle, an
# IDE's JVM) without touching their command lines.
#
# Every certificate gets the alias puddle-<cksum of its body>. A run adds the missing ones and
# deletes puddle-* entries that are no longer in the extra CAs, so a rotated CA does not pile up.
# A state file (extra CAs' checksum, cacerts path, cacerts checksum) makes the next boot skip a
# store that already matches; a Java package update that rebuilds cacerts changes its checksum
# and the import runs again.
#
# This step never fails the boot: a store that is read-only, locked with another password or
# missing is reported on stdout and skipped. POSIX sh; needs awk and cksum.
#
# PUDDLE_ROOT relocates the fixed paths below, for the unit tests only (boot.sh passes it on empty).
ROOT=${PUDDLE_ROOT:-}
EXTRA=$ROOT/etc/puddle/extra-cas.pem
STATE=$ROOT/etc/puddle/java-trust.state
PASS=changeit

# keytool prints "Picked up ..." on stderr and might translate its output; neither is wanted.
unset JAVA_TOOL_OPTIONS JDK_JAVA_OPTIONS _JAVA_OPTIONS
LC_ALL=C
export LC_ALL

say() { printf 'java-trust: %s\n' "$*"; }

[ -s "$EXTRA" ] || {
    say "no extra CAs, nothing to do"
    exit 0
}
for tool in awk cksum readlink; do
    command -v "$tool" >/dev/null 2>&1 || {
        say "skipped ($tool is not in this image)"
        exit 0
    }
done

# Every keytool the image has: the one on PATH, JAVA_HOME's, then the usual JDK folders.
keytools=
for k in "$(command -v keytool 2>/dev/null || true)" "${JAVA_HOME:+$JAVA_HOME/bin/keytool}" \
    "$ROOT"/usr/lib/jvm/*/bin/keytool "$ROOT"/opt/java/*/bin/keytool \
    "$ROOT"/opt/jdk*/bin/keytool "$ROOT"/usr/local/openjdk-*/bin/keytool; do
    [ -n "$k" ] && [ -x "$k" ] || continue
    # With PUDDLE_ROOT set (tests) a Java outside it is the test host's own: never touch it.
    case "$(readlink -f "$k")" in
    "$ROOT"/*) keytools="$keytools
$k" ;;
    esac
done
if [ -z "$keytools" ]; then
    say "no Java in this image"
    exit 0
fi

tmp=$(mktemp -d 2>/dev/null) || {
    say "skipped (no temporary folder)"
    exit 0
}
trap 'rm -rf "$tmp"' EXIT

# One file per certificate: $tmp/certs/<alias>.pem.
mkdir "$tmp/certs"
awk -v dir="$tmp/certs" '
    { sub(/\r$/, "") }
    /^-----BEGIN CERTIFICATE-----/ { inside = 1; n++; file = dir "/" n ".pem" }
    inside { print > file }
    /^-----END CERTIFICATE-----/ { inside = 0; close(file) }
' "$EXTRA"
: >"$tmp/wanted"
for f in "$tmp"/certs/*.pem; do
    [ -f "$f" ] || continue
    alias=puddle-$(grep -v -- '-----' "$f" | tr -d ' \t\r\n' | cksum | cut -d' ' -f1)
    # The same certificate twice gets one alias.
    grep -qx "$alias" "$tmp/wanted" && continue
    printf '%s\n' "$alias" >>"$tmp/wanted"
    mv "$f" "$tmp/certs/$alias.pem"
done
extra_sum=$(cksum <"$EXTRA" | cut -d' ' -f1)

kt() { # keytool with the one store and password; $1 is the keytool, then its arguments
    bin=$1
    shift
    "$bin" "$@" -keystore "$store" -storepass "$PASS" -J-Xshare:auto -J-XX:TieredStopAtLevel=1 2>"$tmp/err"
}

: >"$tmp/seen"
echo "$keytools" | while IFS= read -r keytool; do
    [ -n "$keytool" ] || continue
    home=$(dirname "$(dirname "$(readlink -f "$keytool")")")
    store=
    for c in "$home/lib/security/cacerts" "$home/jre/lib/security/cacerts"; do
        if [ -f "$c" ]; then
            store=$c
            break
        fi
    done
    [ -n "$store" ] || {
        say "$keytool: no cacerts below $home, skipped"
        continue
    }
    store=$(readlink -f "$store")
    # Two keytools of one JDK (or a symlinked store) are one store.
    grep -qxF "$store" "$tmp/seen" && continue
    printf '%s\n' "$store" >>"$tmp/seen"
    [ -w "$store" ] || {
        say "$store is read-only, skipped"
        continue
    }
    state="$extra_sum $store $(cksum <"$store" | cut -d' ' -f1)"
    if [ -f "$STATE" ] && grep -qxF "$state" "$STATE"; then
        say "$store unchanged"
        continue
    fi
    kt "$keytool" -list >"$tmp/list" || {
        say "$store cannot be listed with the default password, skipped: $(tr '\n' ' ' <"$tmp/err")"
        continue
    }
    present=$(sed -n 's/^\(puddle-[0-9]*\),.*/\1/p' "$tmp/list")
    failed=0
    added=0
    removed=0
    for a in $present; do
        grep -qx "$a" "$tmp/wanted" && continue
        if kt "$keytool" -delete -alias "$a" >/dev/null; then removed=$((removed + 1)); else failed=1; fi
    done
    while IFS= read -r a; do
        echo "$present" | grep -qx "$a" && continue
        if kt "$keytool" -importcert -noprompt -trustcacerts -alias "$a" -file "$tmp/certs/$a.pem" >/dev/null; then
            added=$((added + 1))
        else
            failed=1
        fi
    done <"$tmp/wanted"
    if [ "$failed" -ne 0 ]; then
        say "$store: some changes failed, will retry at the next boot: $(tr '\n' ' ' <"$tmp/err")"
        continue
    fi
    # Keep the other stores' lines, replace this one's.
    {
        [ ! -f "$STATE" ] || grep -vF " $store " "$STATE" || true
        echo "$extra_sum $store $(cksum <"$store" | cut -d' ' -f1)"
    } >"$tmp/state.new"
    mkdir -p "$(dirname "$STATE")"
    cat "$tmp/state.new" >"$STATE"
    say "$store updated, $added added, $removed removed"
done
exit 0
