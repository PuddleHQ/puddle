#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Says when an entry of ci/lab/overrides.txt can be deleted.

Usage: ci/lab-override-check.py [--json FILE] [OVERRIDES_FILE]

Each `name==version` line of the overrides file (default ci/lab/overrides.txt) exists because the lab's mitmproxy
declares a bound on `name` that excludes `version`. This reads the latest mitmproxy release from PyPI
(https://pypi.org/pypi/mitmproxy/json, or FILE in the self-test) and checks every override against the bound that
release declares. Exit codes:
  0  every override is still needed, or PyPI could not be read (the message says which: a third party's outage
     must not turn a scheduled run red)
  1  an override can go: the latest release allows its version, or no longer bounds the package
  2  usage error, an unreadable overrides file, or a bound this script cannot evaluate (fix the script)
When an override can go: set the mitmproxy pin in ci/lab/requirements.in to the release, delete the entry (and the
file when it is empty), and regenerate ci/lab/requirements.txt with the command in its header.
"""
import json
import re
import sys
import urllib.request

PYPI = "https://pypi.org/pypi/mitmproxy/json"


def version_key(text):
    """Numeric release tuple, padded so 6.6 == 6.6.0. Pre-release and local parts are refused."""
    if not re.fullmatch(r"\d+(\.\d+)*", text):
        raise ValueError(f"cannot compare version '{text}'")
    parts = [int(p) for p in text.split(".")]
    return tuple(parts + [0] * (8 - len(parts)))


def allows(spec, version):
    """True when the comma-separated specifier set (`<=6.5.10,>=6.5.0`) accepts `version`."""
    target = version_key(version)
    for clause in (c.strip() for c in spec.split(",") if c.strip()):
        m = re.fullmatch(r"(<=|>=|==|!=|<|>|~=)\s*(\d+(?:\.\d+)*)", clause)
        if not m:
            raise ValueError(f"cannot evaluate bound '{clause}'")
        op, bound = m.groups()
        b = version_key(bound)
        if op == "~=":
            width = len(bound.split(".")) - 1
            ok = target >= b and target[:width] == b[:width]
        else:
            ok = {"<=": target <= b, ">=": target >= b, "==": target == b, "!=": target != b, "<": target < b, ">": target > b}[op]
        if not ok:
            return False
    return True


def declared_bound(requires_dist, name):
    """The specifier text mitmproxy declares for `name` (outside any extra), or None when it declares none."""
    for line in requires_dist or []:
        if "extra ==" in line:
            continue
        m = re.fullmatch(r"\s*([A-Za-z0-9_.-]+)\s*(?:\[[^\]]*\])?\s*\(?([^;)]*)\)?\s*(?:;.*)?", line)
        if m and m.group(1).lower().replace("_", "-") == name.lower().replace("_", "-"):
            return m.group(2).strip()
    return None


def read_overrides(path):
    pins = []
    with open(path, encoding="utf-8") as f:
        for raw in f:
            line = raw.split("#", 1)[0].strip()
            if not line:
                continue
            m = re.fullmatch(r"([A-Za-z0-9_.-]+)==(\d+(?:\.\d+)*)", line)
            if not m:
                raise ValueError(f"{path}: cannot read override '{line}'")
            pins.append(m.groups())
    return pins


def fetch(source):
    if source != PYPI:
        with open(source, encoding="utf-8") as f:
            return json.load(f)
    last = None
    for _ in range(3):
        try:
            with urllib.request.urlopen(source, timeout=20) as r:
                return json.load(r)
        except Exception as e:  # network, HTTP or JSON error: all one outcome, retried then failed open
            last = e
    raise OSError(str(last))


def main(argv):
    source, path = PYPI, "ci/lab/overrides.txt"
    args = list(argv)
    if args[:1] == ["--json"] and len(args) >= 2:
        source, args = args[1], args[2:]
    if len(args) > 1:
        print(__doc__, file=sys.stderr)
        return 2
    if args:
        path = args[0]
    try:
        pins = read_overrides(path)
    except (OSError, ValueError) as e:
        print(f"lab-override-check: {e}", file=sys.stderr)
        return 2
    try:
        meta = fetch(source)
        release = meta["info"]["version"]
        requires = meta["info"].get("requires_dist")
    except (OSError, ValueError, KeyError, TypeError) as e:
        print(f"lab-override-check: could not read mitmproxy's latest release ({e}); nothing checked this run, "
              "the next scheduled run tries again")
        return 0
    status = 0
    for name, version in pins:
        try:
            bound = declared_bound(requires, name)
            free = bound is None or bound == "" or allows(bound, version)
        except ValueError as e:
            print(f"lab-override-check: {e}", file=sys.stderr)
            return 2
        if free:
            why = "declares no bound on it" if not bound else f"declares '{bound}', which allows it"
            print(f"lab-override-check: mitmproxy {release} {why}: the override {name}=={version} in {path} can go. "
                  f"Pin mitmproxy=={release} in ci/lab/requirements.in, delete the entry and regenerate "
                  "ci/lab/requirements.txt with the command in its header.")
            status = 1
        else:
            print(f"lab-override-check: mitmproxy {release} still declares '{bound}': the override {name}=={version} is needed")
    return status


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
