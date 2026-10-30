#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
# Tests ci/lab/warm-mitmdump.py with fake mitmdump programs: one that listens after a moment
# (the time is reported and the process is stopped), one that exits at once and one that never
# says anything (both fail with its log, the silent one at the limit).
set -eu

here=$(cd "$(dirname "$0")" && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail() { echo "warm-mitmdump-test: FAIL: $1" >&2; exit 1; }

cat >"$tmp/listens" <<'STUB'
#!/bin/sh
sleep 0.3
echo "HTTP(S) proxy listening at 127.0.0.1:39551."
echo $$ >"$FAKE_DIR/pid"
exec sleep 60
STUB
printf '#!/bin/sh\necho "boom: no module named mitmproxy" >&2\nexit 3\n' >"$tmp/dies"
printf '#!/bin/sh\nexec sleep 60\n' >"$tmp/silent"
chmod +x "$tmp/listens" "$tmp/dies" "$tmp/silent"
export FAKE_DIR="$tmp"

out=$(python3 -I "$here/warm-mitmdump.py" "$tmp/listens") || fail "a listening mitmdump failed the warm-up"
case "$out" in "first mitmdump start took "[0-9]*" s") ;; *) fail "unexpected report: $out" ;; esac
pid=$(cat "$tmp/pid")
if kill -0 "$pid" 2>/dev/null; then fail "the warm-up left mitmdump ($pid) running"; fi

if err=$(python3 -I "$here/warm-mitmdump.py" "$tmp/dies" 2>&1); then fail "an exiting mitmdump passed"; fi
case "$err" in *"exited early (3)"*"no module named mitmproxy"*) ;; *) fail "the early exit lost its log: $err" ;; esac

# The limit is a module constant; shorten it for the silent one.
if err=$(python3 -I -c "
import importlib.util, sys
spec = importlib.util.spec_from_file_location('w', sys.argv[1])
w = importlib.util.module_from_spec(spec); spec.loader.exec_module(w)
try:
    w.warm(sys.argv[2], limit_s=1.0)
except RuntimeError as e:
    print(e); sys.exit(1)
" "$here/warm-mitmdump.py" "$tmp/silent" 2>&1); then fail "a silent mitmdump passed"; fi
case "$err" in *"did not listen within 1 s"*) ;; *) fail "the silent one was not named: $err" ;; esac
echo "warm-mitmdump-test: ok"
