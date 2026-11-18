#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Clones a repository into a workspace so that a clone that dies part way leaves nothing in the
# checkout's place.
#
# Usage: sh clone.sh <url> <stage> <checkout>
# Runs as the session user. Git writes into <stage>, a folder of Puddle's own below the volume's
# `.puddle` (named after the checkout), and only a finished clone is moved to <checkout>, on the
# same volume, in one rename. A clone the VM's death cut short (Puddle killed, a power cut) leaves
# its stage, which the next clone of that checkout removes first; the checkout's place stays
# empty, so Clone again works. Puddle never clones into a folder that holds anything: the script
# refuses when <checkout> exists, before and after the clone.
#
# Exit: git's own code when the clone fails; 3 when <checkout> exists.
set -eu

url=$1
stage=$2
checkout=$3

taken() {
    if [ -e "$checkout" ] || [ -L "$checkout" ]; then
        printf '%s already exists; Puddle never clones into a folder that holds anything\n' "$checkout" >&2
        exit 3
    fi
}

taken
rm -rf -- "$stage"
mkdir -p -- "${stage%/*}"
git clone -- "$url" "$stage"
taken
mv -- "$stage" "$checkout"
