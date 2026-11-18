#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# Every download in CI survives a host that is down for a while (docs/STANDARDS.md, "External downloads in CI"). This
# gate flags, in the workflows, the composite actions and ci/, a download command that has no retry of its own:
#   curl            without --retry (ci/fetch-retry.sh's download and download_from are the usual way)
#   wget            without --tries
#   pip install     without --retries (pip's default gives up after 5 quick tries, seconds apart)
#   Invoke-WebRequest / Invoke-RestMethod without -MaximumRetryCount
# And a ci/fetch-*.sh script whose download host is not GitHub must have a second source: a mirror (`download_from`)
# or the pin-keyed cache (`cache_get`), both in ci/fetch-retry.sh.
# A command split over lines with a trailing backslash (or a PowerShell backtick) is read as one line.
# A command that needs no retry (a probe whose failure is the answer, a local URL, the retry loop itself) says
# `fetch-ok: <reason>` on one of its lines or in the comment line right above it. Other comments are skipped.
#   scripts/check-external-fetch.sh              scan the repository
#   scripts/check-external-fetch.sh --self-test  prove the scan on throwaway files
set -euo pipefail

# Prints `file:line: text` for every offending command of the files named on stdin, one path per line.
scan() {
    local f
    while IFS= read -r f; do
        [ -f "$f" ] || continue
        case $f in */check-external-fetch.sh) continue ;; esac # its self-test fixtures hold the shapes on purpose
        awk -v file="$f" '
            function flush() {
                if (cmd == "") return
                c = cmd
                if (c !~ /fetch-ok:/ && !marked) {
                    bad = ""
                    if (c ~ /(^|[^A-Za-z0-9_$.\/-])curl([.]exe)?[ \t]+-/ || c ~ /(^|[^A-Za-z0-9_$.\/-])curl([.]exe)?[ \t]+["$h]/) {
                        if (c !~ /--retry[ =]/) bad = "curl without --retry"
                    }
                    if (bad == "" && c ~ /(^|[^A-Za-z0-9_$.\/-])wget[ \t]/ && c !~ /--tries/) bad = "wget without --tries"
                    if (bad == "" && c ~ /pip3?"?[ \t]+install([ \t]|$)/ && c !~ /--retries[ =]/) bad = "pip install without --retries"
                    if (bad == "" && c ~ /Invoke-(WebRequest|RestMethod)/ && c !~ /-MaximumRetryCount/) bad = "Invoke-WebRequest without -MaximumRetryCount"
                    if (bad != "") printf "%s:%d: %s: %s\n", file, start, bad, substr(first, 1, 160)
                }
                cmd = ""
            }
            {
                line = $0
                if (cmd == "") {
                    if (line ~ /^[ \t]*#/) { prevmark = (line ~ /fetch-ok:/); next }
                    start = NR; first = line; marked = prevmark; prevmark = 0
                }
                cont = (line ~ /\\[ \t]*$/ || line ~ /`[ \t]*$/)
                sub(/\\[ \t]*$/, "", line); sub(/`[ \t]*$/, "", line)
                cmd = cmd " " line
                if (!cont) flush()
            }
            END { flush() }
        ' "$f"
    done
}

# Prints `file: <host> ...` for every fetch script named on stdin that downloads from a host other than GitHub with
# neither a mirror nor a cache.
second_source() {
    local f hosts pins
    while IFS= read -r f; do
        [ -f "$f" ] || continue
        # The script's own URLs and those in the pin files it reads (ci/vscode.sha256 holds its URLs).
        pins=$(grep -oE '[a-z0-9-]+\.sha256' "$f" | sort -u | sed "s|^|$(dirname "$f")/|" || true)
        # shellcheck disable=SC2086 # pins is a list of paths without spaces, split on purpose
        hosts=$(cat "$f" $pins 2>/dev/null | grep -vE '^[[:space:]]*#' | grep -oE 'https://[A-Za-z0-9.-]+' | sed 's|https://||' |
            grep -vxE 'github\.com|raw\.githubusercontent\.com|objects\.githubusercontent\.com' | sort -u | tr '\n' ' ' || true)
        [ -n "$hosts" ] || continue
        grep -qE '(^|[^_a-z])(download_from|cache_get) ' "$f" && continue
        echo "$f: downloads from ${hosts% }, not GitHub, with no mirror (download_from) or cache (cache_get)"
    done
}

if [ "${1:-}" = --self-test ]; then
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/external-fetch.XXXXXX")
    trap 'rm -rf "$tmp"' EXIT
    fail() { echo "check-external-fetch self-test: $*" >&2; exit 1; }
    # shellcheck disable=SC2016 # fixture text, not shell
    {
        printf '%s\n' 'curl -fsSL -o x https://example.org/x' >"$tmp/bad-curl.sh"
        printf '%s\n' "curl -fsSL \\" '  -o x https://example.org/x' >"$tmp/bad-curl-split.sh"
        printf '%s\n' '      - run: |' '          curl -sS "$URL" -o out' >"$tmp/bad-curl.yml"
        printf '%s\n' 'wget -q https://example.org/x' >"$tmp/bad-wget.sh"
        printf '%s\n' '# fetch-ok: a probe' 'true' 'curl -fsSL -o x https://example.org/x' >"$tmp/bad-curl-stale-mark.sh"
        printf '%s\n' '      - run: pip install -r requirements.txt' >"$tmp/bad-pip.yml"
        printf '%s\n' '          "$RUNNER_TEMP/venv/bin/pip" install --require-hashes -r r.txt' >"$tmp/bad-pip-path.yml"
        printf '%s\n' '          python.exe -m pip install --quiet -r r.txt' >"$tmp/bad-pip-m.yml"
        printf '%s\n' 'Invoke-WebRequest -Uri $u -OutFile $o' >"$tmp/bad-iwr.ps1"
        printf '%s\n' 'Invoke-WebRequest -Uri $u `' '  -OutFile $o' >"$tmp/bad-iwr-split.ps1"
        printf '%s\n' 'curl -fsSL --retry 5 --retry-all-errors -o x https://example.org/x' \
            "curl -fsSL \\" '  --retry 5 -o x https://example.org/x' \
            '# curl -fsSL https://example.org/x' \
            'curl -s --max-time 5 http://127.0.0.1:1/ # fetch-ok: a local probe, the failure is the answer' \
            '# fetch-ok: a probe' 'curl -s -o /dev/null https://example.org/' \
            'command -v curl >/dev/null' 'curl_bin=${CURL:-curl}' 'echo "use curl to fetch"' \
            'wget --tries=5 -q https://example.org/x' \
            'pip install --retries 10 -r r.txt' \
            'Invoke-WebRequest -Uri $u -OutFile $o -MaximumRetryCount 5 -RetryIntervalSec 10' >"$tmp/good.sh"
    }
    for b in bad-curl.sh bad-curl-stale-mark.sh bad-curl-split.sh bad-curl.yml bad-wget.sh bad-pip.yml bad-pip-path.yml bad-pip-m.yml bad-iwr.ps1 bad-iwr-split.ps1; do
        [ -n "$(echo "$tmp/$b" | scan)" ] || fail "$b was not flagged"
    done
    out=$(echo "$tmp/good.sh" | scan)
    [ -z "$out" ] || fail "good.sh was flagged: $out"
    # shellcheck disable=SC2016 # fixture text, not shell
    {
        printf '%s\n' 'url=${X:-https://codeberg.org/x}' 'download l "$url" out 60' >"$tmp/fetch-bad.sh"
        printf '%s\n' 'url=${X:-https://codeberg.org/x}' 'download_from l out 60 "$url" "$mirror"' >"$tmp/fetch-mirror.sh"
        printf '%s\n' 'url=https://download.example.org/x' 'cache_get l x "$want" out || download l "$url" out 60' >"$tmp/fetch-cache.sh"
        printf '%s\n' 'line=$(grep x "$here/tool.sha256")' 'download l "$url" out 60' >"$tmp/fetch-pinurl.sh"
        printf '%s\n' 'tool 1.0 https://update.example.com/1.0/x 0000' >"$tmp/tool.sha256"
        printf '%s\n' '# https://codeberg.org/ in a comment' 'download l "https://github.com/o/r/releases/download/x" out 60' >"$tmp/fetch-github.sh"
    }
    [ -n "$(echo "$tmp/fetch-pinurl.sh" | second_source)" ] || fail "a URL in the pin file was not seen"
    [ -n "$(echo "$tmp/fetch-bad.sh" | second_source)" ] || fail "a non-GitHub fetch with no second source was not flagged"
    for g in fetch-mirror.sh fetch-cache.sh fetch-github.sh; do
        [ -z "$(echo "$tmp/$g" | second_source)" ] || fail "$g was flagged"
    done
    echo "check-external-fetch self-test: ok"
    exit 0
fi

cd "$(dirname "$0")/.."
hits=$(git ls-files -- '.github/workflows/*.yml' '.github/workflows/*.yaml' '.github/actions/**' 'ci/**' |
    grep -E '\.(ya?ml|sh|ps1|psm1|bash)$' | grep -vE -- '-test\.(sh|ps1)$' | scan)
more=$(git ls-files -- 'ci/fetch-*.sh' | grep -v -- '-test\.sh$' | second_source)
[ -z "$more" ] || hits=${hits:+$hits$'\n'}$more
if [ -n "$hits" ]; then
    echo "a download without its own retry (docs/STANDARDS.md, External downloads in CI); add the retry flag, use ci/fetch-retry.sh, or say why not with 'fetch-ok: <reason>':" >&2
    echo "$hits" >&2
    exit 1
fi
echo "external fetches ok"
