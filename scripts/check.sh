#!/bin/sh
# SPDX-License-Identifier: AGPL-3.0-or-later
# The quality gates, in one place: git hooks, CI and humans all run this script, so the gates
# can't drift apart. Usage: scripts/check.sh <gate>...   Gates:
#   fmt, typos, spdx, shellcheck, hooks, git-env, platform-literals, standalone, git-pins, mutants-exclusions, silent-skips, quality, secrets, clippy, clippy-windows, deny, advisories, notices, openapi, test, doc, coverage,
#   coverage-ratchet, diff-coverage, diff-coverage-fast, coverage-determinism, mutants,
#   ui, ui-licences, ui-audit, ui-e2e
#   fast     = fmt typos spdx shellcheck hooks platform-literals standalone git-pins mutants-exclusions silent-skips   (pre-commit hook)
#   all      = fast quality git-env secrets clippy clippy-windows deny advisories notices openapi ui ui-licences ui-audit ui-e2e doc coverage
#              (CI, the nightly run, by hand; coverage runs the tests, and must follow `ui`: the embedded UI is built there)
#   pre-push = every gate of `all` except ui-e2e, the slowest one, and advisories and ui-audit, which depend on
#              the day's advisory databases and not on the change, plus diff-coverage-fast right after the fast gates
#              (pre-push hook; CI still runs ui-e2e after the push)
#   `scripts/check.sh --list <set>` prints a set's gates, one per line, without running anything.
# Several gates run at the same time (scripts/gate-sched.sh): after the `fast` gates, which run first and
# stop the run on a failure, the compile-and-test gates, the UI chain (ui ui-licences ui-audit ui-e2e) and
# secrets/deny/notices are three streams; coverage waits for `ui`. Each gate's output is printed whole under
# its `==> <gate>` line when it finishes; a failure in any stream fails the run (first failure in run order,
# its exit status). PUDDLE_CHECK_SERIAL=1 runs them one by one, as before. A gate set that fits one stream
# runs inline and prints live. The `notices` gate is skipped when its inputs are byte for byte those of a
# passing run (CHECK_NO_SKIP=1 runs it anyway); CI keeps that stamp in a cache entry (CHECK_STAMP_DIR, --stamp-key).
# The ui gates need Node 24 (the UI's package-lock.json pins every npm package). ui-e2e also needs
# the Playwright browsers: `cd ui && npx playwright install --with-deps chromium webkit`.
# Set CARGO to run every Cargo command through a wrapper, e.g. CARGO=mbx for the shared build
# cache (docs/STANDARDS.md, "Shared build cache"); unset, it runs plain cargo. The coverage gates
# ignore it: they always run plain cargo (see `plain_cargo` below).
# Coverage (docs/STANDARDS.md, "Coverage"): the floors live here, the ratchet baseline in
# scripts/coverage-baseline.json; raise them, never lower them without the owner's OK. The ratchet
# lets a total measure up to 0.05 points below its baseline (run-to-run wobble); the floors are exact.
# The crates in scripts/full-coverage-crates.txt must keep every line covered (gate `coverage`).
# The UI has its own ratchet, ui/coverage-baseline.json, which is also vitest's threshold (gate `ui`).
# A change's added lines must also be covered (gates `coverage` for Rust, `ui` for the UI,
# `diff-coverage` by hand); DIFF_BASE names the commit to measure from (CI sets it to the PR or push
# base), else the merge base with origin/develop.
set -eu

COV_LINES=92
COV_REGIONS=90
# Unset it so the wrapper itself (mbx looks up `$CARGO` to find Cargo) doesn't call itself.
cargo="${CARGO:-cargo}"
unset CARGO
# The coverage gates never use the wrapper: a shared cache restores instrumented objects built in another
# checkout, so from the second checkout on the report counts that checkout's files too and the ratchet
# fails (about 3 points of lines too low with mbx 1.22). Plain cargo is right in every checkout.
# With mbx's Cargo shim active (`mbx setup`), `cargo` on PATH is the shim, so look the real Cargo up
# past the shim's directory and mise's command-wrappers directory, where the shim is installed.
plain_cargo=cargo
ifs_saved=$IFS
IFS=:
for dir in $PATH; do
    [ -n "$dir" ] && [ -x "$dir/cargo" ] || continue
    [ ! -e "$dir/mbx-target" ] || continue
    case $dir in */command-wrappers/bin) continue ;; esac
    plain_cargo=$dir/cargo
    break
done
IFS=$ifs_saved

cd "$(dirname "$0")/.."

# Local runs build without debug info: the default dev and test profile writes full DWARF, which is most
# of every test executable (a full workspace test build links about 200 of them and fills tens of GB).
# No gate reads it. A panic message still names file:line; only RUST_BACKTRACE frames lose their lines.
# CI keeps the default. A value already set wins: CARGO_PROFILE_DEV_DEBUG=line-tables-only for frame
# lines, =2 for a debugger.
if [ -z "${CI:-}" ]; then
    export CARGO_PROFILE_DEV_DEBUG="${CARGO_PROFILE_DEV_DEBUG:-0}"
    export CARGO_PROFILE_TEST_DEBUG="${CARGO_PROFILE_TEST_DEBUG:-0}"
fi

# The directory of a concurrent run's per-gate logs (scripts/gate-sched.sh); unset when gates run inline.
run_tmp=

# The desktop shell (puddle-app) links the system WebKitGTK on Linux. Where its dev package is
# missing and this is not CI, the Linux gates skip that one crate and say so (clippy-windows still
# checks it for the msvc target). In CI (CI set) a missing package fails the build instead.
skip_app=
if [ "$(uname -s)" = Linux ] && [ -z "${CI:-}" ] && ! pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
    skip_app="--exclude puddle-app"
    echo "note: libwebkit2gtk-4.1-dev is not installed; skipping puddle-app in the Linux gates" \
        "(sudo apt install libwebkit2gtk-4.1-dev). CI builds and tests it." >&2
fi

# Prints the commit a change is measured from for the diff gates ($DIFF_BASE, else the merge base with
# origin/develop; scripts/diff-base.sh says how a pull request's stale base is handled). Prints nothing
# when neither exists.
diff_base() { scripts/diff-base.sh; }

# Sets COVERAGE_BASELINE_LOWER_OK for the ratchets: $1 is the diff base; 1 only when a commit of the
# change carries the owner's trailer (see the Coverage note above).
lower_ok() {
    COVERAGE_BASELINE_LOWER_OK=0
    if [ -n "$1" ] && git log "$1..HEAD" --format=%B | grep -q '^Owner-OK: coverage-baseline$'; then
        COVERAGE_BASELINE_LOWER_OK=1
    fi
    export COVERAGE_BASELINE_LOWER_OK
}

# Removes what a coverage report would read from an earlier run in the shared instrumented directory: the test
# executables (the libraries stay built) and the profile files. `cargo llvm-cov nextest --no-report` cleans neither.
coverage_scrub() {
    find "$1/llvm-cov-target/debug" -maxdepth 2 -type f -perm -u+x ! -name '*.so' ! -name '*.d' -delete 2>/dev/null || true
    find "$1/llvm-cov-target" -maxdepth 1 -name '*.profraw' -type f -delete 2>/dev/null || true
}
# Unlinks the read-only files of the instrumented directory (`<target>/llvm-cov-target`) before plain cargo builds in it.
# mbx restores a cached output as a read-only hard link to its store object, and rustc stops with `output file ... is not
# writable` when it must rebuild one of them. Unlinking drops only this directory's name for the file: the store object
# and the other checkouts' links keep their mode and content (never chmod it: rustc would then write into the shared
# object). Cargo rebuilds what is missing. A directory only plain cargo has built holds none, so this finds nothing.
coverage_unlock() {
    [ -d "$1/llvm-cov-target" ] || return 0
    locked=$(find "$1/llvm-cov-target" -type f ! -perm -u+w -print -delete 2>/dev/null | grep -c . || true)
    [ "${locked:-0}" = 0 ] || echo "coverage: unlinked $locked read-only file(s) in $1/llvm-cov-target (mbx cache links); cargo rebuilds them" >&2
}
# Runs the diff-coverage script for one language: $1 label, $2 lcov file, $3 extensions, then
# extra arguments. Needs a base; without one it fails.
diff_coverage() {
    label=$1 lcov=$2 exts=$3
    shift 3
    base=$(diff_base)
    if [ -z "$base" ]; then
        echo "$label: no base commit to measure the diff from; run \`git fetch origin develop\` (CI: fetch-depth, DIFF_BASE)" >&2
        return 1
    fi
    node ui/scripts/diff-coverage.ts --base "$base" --lcov "$lcov" --exts "$exts" \
        --exclusions scripts/diff-coverage-exclusions --label "$label" "$@"
}

# Installs the UI's npm packages from the lock file, once per run.
ui_installed=
ui_install() {
    [ -z "$ui_installed" ] || return 0
    for tool in node npm; do
        command -v "$tool" >/dev/null 2>&1 || {
            echo "the ui gates need $tool on PATH (Node 24, docs/STANDARDS.md, \"Toolchain\")" >&2
            exit 1
        }
    done
    (cd ui && npm ci --ignore-scripts --no-audit --no-fund)
    ui_installed=1
}

# The production build, which also lists the packages in the bundle for ui-licences. Built once per
# run: `ui`, `ui-licences` and `ui-e2e` read the same build of the same tree, and a second build
# would empty ui/build under a gate that is reading it. (Gates run in their own subshells when they
# run concurrently, so the "built" mark is a file there.)
ui_built=
ui_build() {
    ui_install
    if [ -z "$ui_built" ] && [ ! -e "${run_tmp:-/nonexistent}/ui-built" ]; then
        (cd ui && npm run build)
        ui_built=1
        [ -z "${run_tmp:-}" ] || : >"$run_tmp/ui-built"
    fi
}

# Where a passing gate leaves its stamp. CI sets CHECK_STAMP_DIR outside target/: rust-cache deletes the
# files of every target subfolder when it saves, so a stamp kept there never reaches the next run.
stamp_dir() {
    printf '%s\n' "${CHECK_STAMP_DIR:-${CARGO_TARGET_DIR:-target}/check-stamps}"
}

# A hash of everything the notices verdict depends on (see the gate). Tracked and untracked files
# alike, by content: a touched file with the same bytes keeps the stamp valid.
notices_inputs_hash() {
    sha=sha256sum
    command -v sha256sum >/dev/null 2>&1 || sha="shasum -a 256"
    # shellcheck disable=SC2086 # $sha is a command and its flag
    {
        cargo-about --version 2>&1 || echo "cargo-about: not installed"
        git ls-files -z --cached --others --exclude-standard -- \
            Cargo.lock about.toml about.hbs ui/package-lock.json rust-toolchain.toml \
            ':(glob)**/Cargo.toml' ':(glob).cargo/**' 'crates/xtask' |
            LC_ALL=C sort -z | xargs -0 $sha 2>&1
    } | $sha | cut -d' ' -f1
}

run_gate() {
    echo "==> $1"
    case "$1" in
    fmt) "$cargo" fmt --all --check ;;
    typos) typos ;;
    spdx) scripts/check-spdx.sh ;;
    shellcheck)
        # Every shell script, including the guest scripts Puddle runs inside sandboxes (POSIX sh).
        command -v shellcheck >/dev/null 2>&1 || {
            echo "shellcheck gate needs shellcheck on PATH (docs/STANDARDS.md, \"Toolchain\")" >&2
            exit 1
        }
        git ls-files -z -- '*.sh' '.githooks/*' | xargs -0 shellcheck
        ;;
    hooks)
        scripts/check-test-git-env.sh --self-test && scripts/check-test-git-env.sh && scripts/pre-push-test.sh && scripts/diff-coverage-crates-test.sh && scripts/mutants-test.sh && scripts/diff-base-test.sh && scripts/check-tmp-test.sh && scripts/cap-test-memory-test.sh && scripts/check-sets-test.sh && scripts/gate-sched-test.sh &&
            scripts/target-names-test.sh --self-test && scripts/target-names-test.sh && scripts/coverage-run-test.sh && scripts/coverage-cargo-test.sh && scripts/coverage-determinism.sh --self-test && scripts/flake-lints-test.sh && scripts/flake-rules-test.sh &&
            ci/fetch-msb-test.sh && ci/fetch-gitleaks-test.sh && ci/fetch-code-server-test.sh && ci/fetch-forgejo-test.sh && ci/fetch-vscode-test.sh && ci/fetch-gateway-test.sh && ci/fetch-tauri-cli-test.sh && ci/fetch-baseline-msi-test.sh && ci/check-runtime-layout-test.sh &&
            ci/scope-test.sh && ci/apt-setup-test.sh && ci/free-disk-test.sh && ci/advisories-issue-test.sh && ci/deny-fresh-test.sh && ci/quality-issue-test.sh && ci/lab-override-check-test.sh && ci/cache-drop-test.sh && ci/disk-watch-test.sh &&
            ci/should-run-test.sh && ci/nightly-issue-test.sh && ci/cancel-group-test.sh && ci/bisect-red-test.sh && ci/cache-gc-test.sh && ci/tools-cache-heal-test.sh && ci/dependabot-auto-merge-test.sh && ci/merge-queue-sweep-test.sh && ci/cancel-dissolved-groups-test.sh && ci/dependabot-notices-test.sh && ci/raise-coverage-baseline-test.sh && ci/test-vm-skip-policy.sh && ci/stress-test.sh && ci/vm-tests-report-test.sh && ci/vm-partition-test.sh &&
            ci/fork-upkeep.py self-test && ci/fork-upkeep.py lint && ci/fork-upkeep-issue-test.sh
        ;;
    git-env)
        # Through the coverage gate's build (same arguments), so it compiles no test binary of its own.
        GIT_ENV_CARGO="$cargo" GIT_ENV_EXCLUDE="$skip_app" scripts/git-env-test.sh
        ;;
    platform-literals)
        # The per-OS runtime file names live in puddle-runtime's table (platform.rs, whose unit tests are
        # platform/tests.rs and assert the table) and nowhere else in the Rust code: a string literal naming msb or its firmware is a hard-coded OS.
        # Versioned names (libkrunfw.so.5.6.1) are msb-release facts, not table entries.
        hits=$(git grep -nE '"(msb\.exe|libkrunfw\.(dll|so(\.5)?|dylib|5\.dylib))"' -- 'crates/*.rs' ':!crates/puddle-runtime/src/platform.rs' ':!crates/puddle-runtime/src/platform/tests.rs' || true)
        if [ -n "$hits" ]; then
            echo "$hits" >&2
            echo "platform-literals: use puddle_runtime::HostOs::runtime_files() instead of these literals" >&2
            exit 1
        fi
        ;;
    standalone) scripts/check-standalone.sh --self-test && scripts/check-standalone.sh && scripts/check-product-name.sh --self-test && scripts/check-product-name.sh ;;
    git-pins) scripts/check-git-pins.sh --self-test && scripts/check-git-pins.sh && ci/image-pins-test.sh && ci/image-pins.sh check ;;
    # An `exclude_re` entry of .cargo/mutants.toml that matches no mutant of `cargo mutants --list` is stale
    # (the code it covered moved): fails, so a position-keyed exclusion cannot rot unseen. A failed
    # listing is an empty list, which the script refuses. `--colors never`: CI sets CARGO_TERM_COLOR=always,
    # which makes cargo-mutants colour the function names inside each line, so no entry matched.
    mutants-exclusions)
        python3 -I scripts/check-mutants-exclusions.py --self-test &&
            "$cargo" mutants --list --no-config --colors never | python3 -I scripts/check-mutants-exclusions.py --list -
        ;;
    # Every `unsafe`, lint suppression, `#[ignore]`, product `unwrap` and TODO must say why in place; a site
    # without a reason fails. No baseline file: a new site with a reason is fine (docs/STANDARDS.md, "Quality").
    quality)
        python3 -I scripts/quality-gate.py --self-test || exit 1
        python3 -I scripts/quality-gate.py --missing . || {
            echo "quality: each site above needs its reason in place (scripts/quality-gate.py header lists the forms)" >&2
            exit 1
        }
        ;;
    # A test that returns early because a prerequisite is missing passes without testing; this fails
    # the hidden forms (docs/STANDARDS.md, "Skipped tests").
    silent-skips) scripts/check-silent-skips.sh --self-test && scripts/check-silent-skips.sh ;;
    secrets)
        # gitleaks with the reviewed allowlist (.gitleaks.toml) over HEAD's commits its base lacks
        # (every ref when CI is set; scripts/check-secrets.sh has the scope rules); needs the pinned
        # gitleaks on PATH (ci/fetch-gitleaks.sh). Not in `fast`: it scans commits.
        scripts/check-secrets.sh --self-test && scripts/check-secrets.sh
        ;;
    clippy)
        # shellcheck disable=SC2086 # $skip_app is empty or one `--exclude <crate>` pair
        "$cargo" clippy --workspace $skip_app --all-targets --all-features --locked -- -D warnings
        ;;
    clippy-windows)
        # cargo-xwin supplies the MSVC headers and libraries, so C dependencies (bundled SQLite)
        # build for the msvc target. It drives clang as clang-cl and the toolchain's llvm-ar as
        # llvm-lib, so it needs `clang` on PATH; the shell's Windows resources need `llvm-rc`. Run through plain cargo: the cross-check doesn't
        # share the native build's cache anyway.
        # Distro LLVM packages (Debian, Ubuntu) keep llvm-rc and friends in /usr/lib/llvm-N/bin, off
        # PATH; use the newest one when llvm-rc isn't already on PATH.
        if ! command -v llvm-rc >/dev/null 2>&1; then
            for dir in /usr/lib/llvm-*/bin; do
                [ ! -x "$dir/llvm-rc" ] || llvm_bin=$dir
            done
            [ -z "${llvm_bin:-}" ] || PATH="$llvm_bin:$PATH"
        fi
        for tool in cargo-xwin clang llvm-rc; do
            command -v "$tool" >/dev/null 2>&1 || {
                echo "clippy-windows needs $tool on PATH (docs/STANDARDS.md, \"Toolchain\")" >&2
                exit 1
            }
        done
        cargo xwin clippy --workspace --all-targets --all-features --locked \
            --target x86_64-pc-windows-msvc -- -D warnings
        ;;
    # `deny` is what a change can break; `advisories` is what a new RustSec entry can break on a day no one
    # touched the lock file. Pull requests to develop run only `deny` (ci.yml); `advisories` runs on pull
    # requests to main, on pushes to develop and nightly, where it also keeps one tracking issue.
    deny) "$cargo" deny --locked check bans licenses sources ;;
    # ci/deny-fresh.sh fetches the advisory database and registry index first, so a crate yanked since the last
    # fetch fails here as it does in the scheduled job (and says so when the database was over a day old).
    advisories) DENY="$cargo deny" ci/deny-fresh.sh --locked check advisories ;;
    notices)
        # Every shipped dependency has a licence entry in the third-party notices (cargo-about).
        # The verdict is a function of the dependency manifests and lock, the cargo-about config, the
        # xtask that judges them and the cargo-about version, so a run that passed on the same bytes
        # is not repeated. Stamp: a hash in the target dir, written only by a pass.
        stamp="$(stamp_dir)/notices"
        key=$(notices_inputs_hash)
        if [ -z "${CHECK_NO_SKIP:-}" ] && [ "$(cat "$stamp" 2>/dev/null)" = "$key" ]; then
            echo "notices: skipped, its inputs are byte for byte those of the last passing run ($key); CHECK_NO_SKIP=1 runs it anyway"
            return 0
        fi
        # --offline: crawl only the local registry sources, no network; the report is the same bytes
        # as the online one. A cold registry cache (CI without a cache hit) lacks sources, so then
        # the online run decides.
        "$cargo" run --quiet --locked -p xtask -- notices --check --offline || {
            echo "notices: the offline run did not pass; running it online" >&2
            "$cargo" run --quiet --locked -p xtask -- notices --check
        }
        mkdir -p "$(dirname "$stamp")"
        printf '%s' "$key" >"$stamp"
        ;;
    openapi)
        # The committed API contract (openapi.json, schema.d.ts) matches the routes (ADR 0004).
        # The TypeScript half needs Node; without it locally the gate says so and checks the JSON
        # only. In CI (CI set) a missing Node fails.
        "$cargo" run --quiet --locked -p xtask -- openapi --check
        ;;
    ui)
        # format, lint, types (Svelte's a11y warnings fail), unit and component tests with the
        # coverage thresholds in ui/vite.config.ts, then the production build and its size budget.
        ui_install
        (cd ui && npm run format:check && npm run lint && npm run check && npm test)
        # The UI's own ratchet: the thresholds are ui/coverage-baseline.json; a local run raises it, CI only compares.
        base=$(diff_base)
        lower_ok "$base"
        ui_ratchet="--summary ui/coverage/coverage-summary.json --baseline ui/coverage-baseline.json"
        [ -n "${CI:-}" ] || ui_ratchet="$ui_ratchet --write"
        if [ -n "$base" ] && git cat-file -e "$base:ui/coverage-baseline.json" 2>/dev/null; then
            git show "$base:ui/coverage-baseline.json" >ui/coverage/coverage-baseline.previous.json
            ui_ratchet="$ui_ratchet --previous ui/coverage/coverage-baseline.previous.json"
        fi
        # shellcheck disable=SC2086 # $ui_ratchet is a list of flags, split on purpose
        node ui/scripts/ui-coverage-ratchet.ts $ui_ratchet
        # vitest's lcov paths are relative to ui/; the exclusion file's paths are repository-relative.
        diff_coverage "diff-coverage (ui)" ui/coverage/lcov.info .ts,.svelte --prefix ui/
        ui_build
        # What the first page load fetches stays under the gzip budget (ui/scripts/bundle-budget.ts).
        (cd ui && node scripts/bundle-budget.ts)
        ;;
    ui-licences)
        # Every npm package in the bundle is on the licence allowlist and in the UI notices.
        ui_build
        (cd ui && node scripts/licences.ts --check)
        ;;
    ui-audit)
        # npm audit on production dependencies, minus ui/audit-exceptions.json. Like `advisories`, it answers
        # for the day's database, so pull requests to develop skip it (ci.yml, job advisories-npm).
        ui_install
        (cd ui && node scripts/audit.ts)
        ;;
    ui-e2e)
        # Playwright against the real API serving the built UI. WebKit stands in for WebKitGTK and
        # WKWebView; where it can't start locally (missing system libraries) this runs Chromium
        # only and says so. CI (CI set) never narrows by itself; PUDDLE_E2E_PROJECTS set by the
        # caller (ci.yml's `ui` job runs Chromium, its `ui-webkit` job WebKit) narrows anywhere.
        ui_build
        case "$(uname -s)" in
        MINGW* | MSYS* | CYGWIN*) ;; # Windows runs the installed Edge, not Playwright's browsers
        *)
            if [ -z "${CI:-}" ] && [ -z "${PUDDLE_E2E_PROJECTS:-}" ] && ! (cd ui && node scripts/webkit-probe.ts); then
                echo "note: WebKit can't start here; running Chromium only (to add it: cd ui && npx playwright install --with-deps webkit)" >&2
                PUDDLE_E2E_PROJECTS=chromium
                export PUDDLE_E2E_PROJECTS
            fi
            ;;
        esac
        (cd ui && PUDDLE_CARGO="$cargo" npx playwright test)
        ;;
    test)
        # shellcheck disable=SC2086 # $skip_app is empty or one `--exclude <crate>` pair
        "$cargo" nextest run --workspace $skip_app --all-features --locked
        ;;
    doc)
        # shellcheck disable=SC2086 # $skip_app is empty or one `--exclude <crate>` pair
        "$cargo" test --doc --workspace $skip_app --all-features --locked --no-fail-fast
        # shellcheck disable=SC2086 # $skip_app is empty or one `--exclude <crate>` pair
        RUSTDOCFLAGS="-D warnings" "$cargo" doc --workspace $skip_app --no-deps --all-features --locked
        ;;
    coverage | coverage-ratchet)
        # The tests with coverage (unreadable profile files set aside), the reports, then the ratchet (floors above, baseline committed) and the
        # diff-coverage gate on the lcov report. `coverage-ratchet` is the same run; either raises
        # scripts/coverage-baseline.json when coverage went up (not in CI; do not commit it, the nightly
        # coverage-baseline workflow raises the baseline in its own pull request).
        ui_install
        target=${CARGO_TARGET_DIR:-target}
        coverage_unlock "$target"
        # shellcheck disable=SC2086 # $skip_app is empty or one `--exclude <crate>` pair
        scripts/coverage-run.sh "$plain_cargo" --workspace $skip_app --all-features --locked \
            --profile "${NEXTEST_PROFILE:-default}"
        "$plain_cargo" llvm-cov report --lcov --output-path "$target/lcov.info"
        "$plain_cargo" llvm-cov report --summary-only
        "$plain_cargo" llvm-cov report --json --summary-only --output-path "$target/coverage-summary.json"
        ratchet="--summary $target/coverage-summary.json --baseline scripts/coverage-baseline.json"
        ratchet="$ratchet --floor-lines $COV_LINES --floor-regions $COV_REGIONS"
        [ -n "${CI:-}" ] || ratchet="$ratchet --write"
        base=$(diff_base)
        # Lowering the committed baseline needs the owner's OK, recorded as this trailer in a commit
        # message of the change; the environment can't grant it.
        lower_ok "$base"
        if [ -n "$base" ] && git cat-file -e "$base:scripts/coverage-baseline.json" 2>/dev/null; then
            git show "$base:scripts/coverage-baseline.json" >"$target/coverage-baseline.previous.json"
            ratchet="$ratchet --previous $target/coverage-baseline.previous.json"
        fi
        # shellcheck disable=SC2086 # $ratchet is a list of flags, split on purpose
        node ui/scripts/coverage-ratchet.ts $ratchet
        # The pure-logic crates stay at 100 % of their lines (the workspace totals can't see one slip).
        node ui/scripts/full-coverage.ts --lcov "$target/lcov.info" \
            --crates scripts/full-coverage-crates.txt --exclusions scripts/full-coverage-exclusions.txt
        diff_coverage "diff-coverage (rust)" "$target/lcov.info" .rs
        ;;
    coverage-determinism)
        # Two coverage runs of the 100 % crates must execute the same lines (slow, not in a gate set; see
        # scripts/coverage-determinism.sh).
        scripts/coverage-determinism.sh "$plain_cargo"
        ;;
    mutants)
        # Mutation testing of the lines this change adds or changes, in the crates of
        # scripts/mutants-crates.txt (cargo-mutants, slow: not in a gate set; the `mutants` workflow runs it
        # on pull requests and nightly). A surviving mutant needs a better test. docs/STANDARDS.md, "Mutation testing".
        scripts/mutants.sh diff "$(diff_base)"
        ;;
    diff-coverage-fast)
        # The Rust half of the diff-coverage gate on the crates the change touches, before the slow gates:
        # their tests with coverage, then the lines the change adds against that report. A line only another
        # crate's tests run shows up as uncovered here; the full `coverage` gate then decides (it measures the
        # whole workspace), so a failure here that the full gate would clear is a line to list or to test
        # from its own crate. Untracked new files are invisible to git diff; `git add -N <file>` first.
        base=$(diff_base)
        if [ -z "$base" ]; then
            echo "diff-coverage-fast: no base commit to measure the diff from; run \`git fetch origin develop\`" >&2
            exit 1
        fi
        crates=$(scripts/diff-coverage-crates.sh "$base")
        if [ -z "$crates" ]; then
            echo "diff-coverage-fast: no Rust file changed; nothing to measure"
            exit 0
        fi
        if [ "$crates" = "*" ]; then
            echo "diff-coverage-fast: a Rust file outside crates/ changed; the coverage gate measures it"
            exit 0
        fi
        packages=
        filter=
        for c in $crates; do
            [ -z "$skip_app" ] || [ "$c" != puddle-app ] || continue
            packages="$packages $c"
            filter="${filter:+$filter | }package($c)"
        done
        if [ -z "$packages" ]; then
            echo "diff-coverage-fast: only puddle-app changed and it is skipped here; CI measures it"
            exit 0
        fi
        # The same instrumented target directory as the `coverage` gate (`<target>/llvm-cov-target`), so the third-party
        # crates it builds are built once, not once per gate (15 GB of a host lane when each kept its own copy). A
        # report reads every profile file and every test executable in the directory, so one left by an earlier run
        # (other crates, older sources) would bring its old line numbers or another run's counts; `coverage_scrub`
        # removes both before this run and again when it ends, so neither gate reports what the other left. Every
        # library stays built. The lcov file is this gate's own, never the `coverage` gate's `lcov.info`.
        target=${CARGO_TARGET_DIR:-target}
        lcov=$target/diff-coverage-fast.lcov
        echo "diff-coverage-fast: tests with coverage for:$packages"
        coverage_unlock "$target"
        coverage_scrub "$target"
        trap 'coverage_scrub "$target"; check_tmp_cleanup' EXIT
        # The build selects the whole workspace like the `coverage` gate (a `-p` selection resolves other features
        # for some third-party crates, so their units would hash differently and be built twice); the filterset
        # picks the tests that run.
        # shellcheck disable=SC2086 # $skip_app is empty or one `--exclude <crate>` pair
        scripts/coverage-run.sh "$plain_cargo" --workspace $skip_app --all-features --locked \
            --profile "${NEXTEST_PROFILE:-default}" -E "$filter"
        "$plain_cargo" llvm-cov report --lcov --output-path "$lcov"
        # The exclusion notes ("matched nothing in this diff") list every entry of the file that this change does
        # not touch: hundreds of lines, so only their count is printed.
        rc=0
        report=$(diff_coverage "diff-coverage-fast (rust)" "$lcov" .rs 2>&1) || rc=$?
        printf '%s\n' "$report" | grep -v 'matched nothing in this diff' || true
        echo "diff-coverage-fast: $(printf '%s\n' "$report" | grep -c 'matched nothing in this diff' || true) exclusion note(s) for untouched code left out"
        if [ "$rc" != 0 ]; then
            echo "diff-coverage-fast: only the tests of$packages ran; a line another crate's tests run is reported here and cleared by the full coverage gate" >&2
            exit "$rc"
        fi
        ;;
    diff-coverage)
        # By hand, after `coverage` and `ui` ran: both reports against the base. Untracked new
        # files are invisible to git diff; `git add -N <file>` first.
        diff_coverage "diff-coverage (rust)" "${CARGO_TARGET_DIR:-target}/lcov.info" .rs
        diff_coverage "diff-coverage (ui)" ui/coverage/lcov.info .ts,.svelte --prefix ui/
        ;;
    *)
        echo "unknown gate: $1" >&2
        exit 2
        ;;
    esac
}

# The gate sets, in run order. `pre-push` is derived from `all`, so a gate added to `all` is in it too.
fast_gates="fmt typos spdx shellcheck hooks platform-literals standalone git-pins mutants-exclusions silent-skips"
all_gates="$fast_gates quality git-env secrets clippy clippy-windows deny advisories notices openapi ui ui-licences ui-audit ui-e2e doc coverage"
# After the fast gates and before the slow ones: a change whose added lines no test runs fails in minutes.
pre_push_gates=$fast_gates" diff-coverage-fast"
for g in $all_gates; do
    case " $fast_gates " in *" $g "*) continue ;; esac
    case $g in ui-e2e | advisories | ui-audit) ;; *) pre_push_gates="$pre_push_gates $g" ;; esac
done

# Prints the gates of a set, or of a single gate, one per line.
gates_of() {
    # shellcheck disable=SC2086 # the sets are space-separated names by design
    case "$1" in
    fast) printf '%s\n' $fast_gates ;;
    all) printf '%s\n' $all_gates ;;
    pre-push) printf '%s\n' $pre_push_gates ;;
    *) printf '%s\n' "$1" ;;
    esac
}

if [ "${1:-}" = --list ]; then
    [ "$#" -ge 2 ] || {
        echo "usage: scripts/check.sh --list <set>..." >&2
        exit 2
    }
    shift
    for arg in "$@"; do gates_of "$arg"; done
    exit 0
fi

# `--stamp-key notices` prints the stamp a passing notices gate writes for the current inputs, so CI can keep
# the stamp in a cache entry named by it (a hit means the gate passed on these bytes).
if [ "${1:-}" = --stamp-key ]; then
    [ "${2:-}" = notices ] && [ "$#" -eq 2 ] || {
        echo "usage: scripts/check.sh --stamp-key notices" >&2
        exit 2
    }
    notices_inputs_hash
    exit 0
fi

[ "$#" -gt 0 ] || set -- all
requested=
for arg in "$@"; do
    requested="$requested $(gates_of "$arg" | tr '\n' ' ')"
done

# Temp dir: cargo, nextest, the tests' fixtures, vitest and Playwright write scratch files to $TMPDIR, by default
# /tmp, which on a host with a RAM-backed tmpfs is small and shared (one gate run filling it fails every other
# process there). A run therefore points TMPDIR at a fresh directory under the target dir, when that is outside the repository (CHECK_TMPDIR=DIR uses
# DIR/chk.* instead), removed when the run ends, also on a failure or a signal. A real run of the test, ui
# and ui-e2e gates (cargo, nextest, vitest, Playwright with Chromium and WebKit) added nothing to /tmp or /dev/shm.
# A tool that hardcodes a path still writes there, and a socket path under a deep target dir can pass the 107 bytes
# of sun_path (the one test that nests two roots uses a short base; mbx binds a socket in TMPDIR, so the directory
# name stays short: <target dir>/chk.XXXXXX/mbx-session-XXXXXX/cache-agent.sock must fit in 107 bytes).
check_tmp=
check_tmp_cleanup() { [ -z "$check_tmp" ] || rm -rf "$check_tmp"; }
private_tmp() {
    base="${CHECK_TMPDIR:-${CARGO_TARGET_DIR:-target}}"
    case $base in /*) ;; *) base="$PWD/$base" ;; esac
    # Not inside the repository: tests that build a scratch repository or crate under TMPDIR would find this one
    # above it (the default target dir `target` is inside, so a plain run keeps the system temp dir).
    case $base in "$PWD" | "$PWD"/*)
        echo "check: the target dir is inside the repository, TMPDIR stays ${TMPDIR:-/tmp} (set CARGO_TARGET_DIR outside it, or CHECK_TMPDIR)" >&2
        return 0
        ;;
    esac
    mkdir -p "$base"
    check_tmp=$(mktemp -d "$base/chk.XXXXXX")
    trap check_tmp_cleanup EXIT
    trap 'check_tmp_cleanup; exit 130' INT
    trap 'check_tmp_cleanup; exit 143' TERM HUP
    export TMPDIR="$check_tmp"
}
private_tmp

# The gates that need the UI's npm packages (the UI itself, and coverage for its ratchet script):
# installed once, before the concurrent streams start, so no two of them run `npm ci` at once.
before_concurrent() {
    for g; do
        case $g in ui | ui-licences | ui-audit | ui-e2e | coverage | coverage-ratchet) ui_install && return 0 ;; esac
    done
}

# shellcheck source=scripts/gate-sched.sh
. scripts/gate-sched.sh
# The serial prefix. diff-coverage-fast is in it, so in the pre-push set it fails the run before the streams start.
serial_gates="$fast_gates diff-coverage-fast"
# shellcheck disable=SC2086 # the gate names are words by design
run_gates $requested
