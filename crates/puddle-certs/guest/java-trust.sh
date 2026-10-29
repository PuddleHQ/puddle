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
# keytool imports one certificate per run and every run is a JVM start (about 0.4 s), so the
# missing certificates are written as one JKS file and imported by a single `keytool
# -importkeystore`: a first boot runs keytool twice per store (list, import), whatever the count.
# A stale entry is still one `keytool -delete` each; a rotation replaces a handful. A certificate
# that is not valid X.509 fails the whole import (the extra CAs file is Puddle's own parsed output).
#
# This step never fails the boot: a store that is read-only, locked with another password or
# missing is reported on stdout and skipped. POSIX sh; needs awk, cksum, readlink, base64, sha1sum and wc.
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
for tool in awk cksum readlink base64 sha1sum wc; do
    command -v "$tool" >/dev/null 2>&1 || {
        say "skipped ($tool is not in this image)"
        exit 0
    }
done

# Every keytool the image has: the one on PATH, JAVA_HOME's, then the usual JDK folders and the
# version managers' (SDKMAN, IntelliJ's ~/.jdks, asdf, mise: for root and for each home below
# /home; only SDKMAN is also looked up under $HOME and SDKMAN_DIR). A JDK unpacked anywhere else, and not on PATH or in
# JAVA_HOME, is not found.
keytools=
for k in "$(command -v keytool 2>/dev/null || true)" "${JAVA_HOME:+$JAVA_HOME/bin/keytool}" \
    "${SDKMAN_DIR:-/nonexistent}"/candidates/java/*/bin/keytool \
    "${HOME:-/nonexistent}"/.sdkman/candidates/java/*/bin/keytool \
    "$ROOT"/usr/lib/jvm/*/bin/keytool "$ROOT"/usr/lib64/jvm/*/bin/keytool \
    "$ROOT"/usr/java/*/bin/keytool "$ROOT"/usr/local/java*/bin/keytool \
    "$ROOT"/opt/java/*/bin/keytool \
    "$ROOT"/opt/jdk*/bin/keytool "$ROOT"/usr/local/openjdk-*/bin/keytool \
    "$ROOT"/usr/local/sdkman/candidates/java/*/bin/keytool \
    "$ROOT"/root/.sdkman/candidates/java/*/bin/keytool \
    "$ROOT"/root/.jdks/*/bin/keytool "$ROOT"/root/.asdf/installs/java/*/bin/keytool \
    "$ROOT"/root/.local/share/mise/installs/java/*/bin/keytool \
    "$ROOT"/home/*/.sdkman/candidates/java/*/bin/keytool \
    "$ROOT"/home/*/.jdks/*/bin/keytool "$ROOT"/home/*/.asdf/installs/java/*/bin/keytool \
    "$ROOT"/home/*/.local/share/mise/installs/java/*/bin/keytool; do
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
    "$bin" "$@" -keystore "$store" -storepass "$PASS" -J-Xshare:auto -J-XX:TieredStopAtLevel=1 </dev/null 2>"$tmp/err"
}

# byte <n>: the one byte n (printf takes it as an octal escape).
byte() {
    # shellcheck disable=SC2059 # the format is the escape built here
    printf "\\$(printf %03o "$1")"
}
be16() {
    byte $(($1 / 256 % 256))
    byte $(($1 % 256))
}
be32() {
    be16 $(($1 / 65536))
    be16 $(($1 % 65536))
}

# write_jks <file> <alias>...: a JKS keystore (the format every keytool reads) holding the
# certificates $tmp/certs/<alias>.pem as trusted entries: magic, version 2, a count, then per entry
# tag 2, the alias, a timestamp, "X.509" and the DER; closed by the SHA-1 of the password (UTF-16BE),
# "Mighty Aphrodite" and everything before it, which Java checks when it loads the file.
write_jks() {
    out=$1
    shift
    {
        printf '\376\355\376\355'
        be32 2
        be32 $#
        for a in "$@"; do
            grep -v -- '-----' "$tmp/certs/$a.pem" | base64 -d >"$tmp/der" || return 1
            be32 2
            be16 ${#a}
            printf %s "$a"
            printf '\000\000\000\000\000\000\000\000'
            be16 5
            printf X.509
            be32 "$(wc -c <"$tmp/der")"
            cat "$tmp/der"
        done
    } >"$tmp/jks.body" || return 1
    hex=$(
        {
            printf '\000c\000h\000a\000n\000g\000e\000i\000t'
            printf 'Mighty Aphrodite'
            cat "$tmp/jks.body"
        } | sha1sum | cut -d' ' -f1
    )
    {
        cat "$tmp/jks.body"
        while [ -n "$hex" ]; do
            pair=${hex%"${hex#??}"}
            hex=${hex#??}
            byte "0x$pair"
        done
    } >"$out"
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
    missing=
    while IFS= read -r a; do
        echo "$present" | grep -qx "$a" || missing="$missing $a"
    done <"$tmp/wanted"
    if [ -n "$missing" ]; then
        # shellcheck disable=SC2086 # aliases are digits and a prefix, one word each
        if write_jks "$tmp/new.jks" $missing &&
            "$keytool" -importkeystore -noprompt -srckeystore "$tmp/new.jks" -srcstoretype JKS \
                -srcstorepass "$PASS" -destkeystore "$store" -deststorepass "$PASS" \
                -J-Xshare:auto -J-XX:TieredStopAtLevel=1 </dev/null >"$tmp/err" 2>&1 &&
            grep -q ' 0 entries failed' "$tmp/err"; then
            # shellcheck disable=SC2086
            added=$(set -- $missing && echo $#)
        else
            failed=1
        fi
    fi
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
