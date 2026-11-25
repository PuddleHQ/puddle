#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# Self-test of check-spdx.sh on a throwaway repository with a two-crate workspace (one crate under
# another licence): matching headers pass; a missing header, a GPL header in an AGPL crate, an AGPL
# header in the other crate, a wrong header outside every crate and a CRLF file each behave right.
set -eu
# check.sh runs under hooks that export GIT_DIR and friends; a leaked one would point `git init` at the real repository.
# shellcheck disable=SC1091 # the helper sits beside this script
. "$(dirname "$0")/git-clean-env.sh"
# shellcheck disable=SC1091 # the helper sits in scripts/
. "$(dirname "$0")/../scripts/test-fail.sh"
here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/check-spdx-test.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
cd "$tmp"
git init -q .
mkdir -p scripts crates/a/src crates/b/src
cp "$here/check-spdx.sh" scripts/
cat >Cargo.toml <<'T'
# SPDX-License-Identifier: AGPL-3.0-or-later
[workspace]
members = ["crates/a", "crates/b"]
resolver = "2"
[workspace.package]
license = "AGPL-3.0-or-later"
T
printf '%s\n' '# SPDX-License-Identifier: AGPL-3.0-or-later' '[package]' 'name = "a"' 'version = "0.1.0"' \
    'edition = "2021"' 'license.workspace = true' >crates/a/Cargo.toml
printf '%s\n' '# SPDX-License-Identifier: MIT' '[package]' 'name = "b"' 'version = "0.1.0"' \
    'edition = "2021"' 'license = "MIT"' >crates/b/Cargo.toml
echo '// SPDX-License-Identifier: AGPL-3.0-or-later' >crates/a/src/lib.rs
echo '// SPDX-License-Identifier: MIT' >crates/b/src/lib.rs
printf '#!/bin/sh\n# SPDX-License-Identifier: AGPL-3.0-or-later\r\n' >run.sh
git add -A

run() { scripts/check-spdx.sh 2>&1; }

run >/dev/null || fail "matching headers failed"

echo '// SPDX-License-Identifier: GPL-3.0-or-later' >crates/a/src/lib.rs
out=$(run) && fail "GPL header in an AGPL crate passed"
case $out in *"crates/a/src/lib.rs says GPL-3.0-or-later, its crate is AGPL-3.0-or-later"*) ;; *) fail "message: $out" ;; esac
echo '// SPDX-License-Identifier: AGPL-3.0-or-later' >crates/a/src/lib.rs

echo '// SPDX-License-Identifier: AGPL-3.0-or-later' >crates/b/src/lib.rs
out=$(run) && fail "AGPL header in the MIT crate passed"
case $out in *"crates/b/src/lib.rs says AGPL-3.0-or-later, its crate is MIT"*) ;; *) fail "message: $out" ;; esac
echo '// SPDX-License-Identifier: MIT' >crates/b/src/lib.rs

echo '// no header' >crates/a/src/lib.rs
out=$(run) && fail "missing header passed"
case $out in *"missing SPDX header: crates/a/src/lib.rs"*) ;; *) fail "message: $out" ;; esac
echo '// SPDX-License-Identifier: AGPL-3.0-or-later' >crates/a/src/lib.rs

echo '# SPDX-License-Identifier: GPL-3.0-or-later' >outside.sh
git add outside.sh
out=$(run) && fail "wrong header outside every crate passed"
case $out in *"outside.sh says GPL-3.0-or-later, its crate is AGPL-3.0-or-later"*) ;; *) fail "message: $out" ;; esac

echo "check-spdx-test: ok"
