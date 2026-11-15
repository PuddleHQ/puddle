#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Usage: ci/with-keyring.sh <command>...
# Runs the command with a working Secret Service: a throwaway D-Bus session and gnome-keyring with its login
# keyring unlocked from stdin (how a headless Linux install unlocks it). Needs the `gnome-keyring` and `dbus`
# packages. Without a session like this, a runner that has those packages installed answers "a Secret Service
# exists" through D-Bus activation and then fails every write on a locked daemon.
# Order matters (measured): `--login` first makes and unlocks login.keyring and prints the control socket;
# `--start` then claims org.freedesktop.secrets on the bus in front of that same daemon. Started the other way
# round, or with `--login --daemonize` alone, the bus activates a second, locked daemon and every write fails.
set -euo pipefail
[ "$#" -gt 0 ] || { echo "usage: ci/with-keyring.sh <command>..." >&2; exit 2; }
# shellcheck disable=SC2016 # the script is for the inner shell to expand
exec dbus-run-session -- bash -c '
    set -euo pipefail
    eval "$(printf %s "ci-keyring-password" | gnome-keyring-daemon --login --components=secrets)"
    export GNOME_KEYRING_CONTROL
    eval "$(gnome-keyring-daemon --start --components=secrets)"
    exec "$@"
' with-keyring "$@"
