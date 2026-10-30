# SPDX-License-Identifier: GPL-3.0-or-later
"""Starts mitmdump once, waits until it listens, stops it and prints how long that took.

The corporate-lab tests start mitmdump from a 60 s bound. The first start out of a new virtual
environment reads thousands of Python files for the first time and the antivirus scans each one:
14 to 29 s measured on a developer Windows machine, 1 s from the second start on. CI runs this
once after the install, outside any test, so that cost is not a test's and the log shows what a
start takes on the runner. `mitmdump --version` is not enough: it imports only part of what a
running proxy loads (4 s for the first start after it, against 1 s warm).

Usage: warm-mitmdump.py <path to mitmdump>
"""

import os
import subprocess
import sys
import tempfile
import time

LIMIT_S = 300.0


def stop(proc: subprocess.Popen) -> None:
    """Stops mitmdump and its children (on Windows the venv launcher starts the real process)."""
    if sys.platform == "win32":
        subprocess.run(
            ["taskkill", "/F", "/T", "/PID", str(proc.pid)],
            check=False,
            capture_output=True,
        )
    else:
        proc.terminate()
    proc.wait(timeout=30)


def warm(binary: str, limit_s: float = LIMIT_S) -> float:
    """Seconds from the start of `binary` to its `proxy listening at` log line.

    Raises RuntimeError with the log when it exits first or says nothing within `limit_s`.
    """
    with tempfile.TemporaryDirectory(prefix="puddle-warm-mitm-") as tmp:
        log_path = os.path.join(tmp, "mitm.log")
        with open(log_path, "wb") as log:
            begun = time.monotonic()
            proc = subprocess.Popen(
                [
                    binary,
                    "--listen-host",
                    "127.0.0.1",
                    "--listen-port",
                    "0",
                    "--set",
                    f"confdir={os.path.join(tmp, 'conf')}",
                    "--set",
                    "termlog_verbosity=info",
                ],
                env={**os.environ, "PYTHONUNBUFFERED": "1"},
                stdin=subprocess.DEVNULL,
                stdout=log,
                stderr=log,
            )
        try:
            while True:
                with open(log_path, encoding="utf-8", errors="replace") as f:
                    text = f.read()
                if "proxy listening at" in text:
                    return time.monotonic() - begun
                if proc.poll() is not None:
                    raise RuntimeError(f"mitmdump exited early ({proc.returncode})\n{text}")
                if time.monotonic() - begun > limit_s:
                    raise RuntimeError(f"mitmdump did not listen within {limit_s:.0f} s\n{text}")
                time.sleep(0.1)
        finally:
            stop(proc)


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        seconds = warm(argv[1])
    except RuntimeError as err:
        print(err, file=sys.stderr)
        return 1
    print(f"first mitmdump start took {seconds:.1f} s")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
