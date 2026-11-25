# shellcheck shell=sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Sourced (`. "$(dirname "$0")/../scripts/test-fail.sh"`) by the *-test.sh scripts: `fail <message>` prints
# `<script name>: FAIL: <message>` on stderr and exits 1.
fail() {
    echo "$(basename "$0" .sh): FAIL: $*" >&2
    exit 1
}
