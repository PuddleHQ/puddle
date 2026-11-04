#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Fails an `exclude_re` entry of .cargo/mutants.toml that matches no mutant.

An exclusion names a mutant by its `cargo mutants --list` line (`file:line:column: description`). Some
cannot be keyed on the function alone: the Windows and the Unix body of one function carry the same name,
so only the position tells them apart. When the code moves, such an entry matches nothing and the mutant
it was meant to cover is measured again (or, worse, a range now covers other code). This gate lists the
mutants without the config and fails every entry that matches none of them, so a moved or deleted
function sends the author back to the entry.

    scripts/check-mutants-exclusions.py --list FILE   FILE (or - for stdin) is the output of `cargo mutants --list --no-config`
    scripts/check-mutants-exclusions.py --self-test   plants a stale entry and shows the check catch it

`scripts/check.sh mutants-exclusions` runs both. Entries are matched with Python's `re`, which reads
every form the file uses the way cargo-mutants' regex engine does.
"""

import re
import sys
import tomllib
from pathlib import Path

CONFIG = Path(".cargo/mutants.toml")


def stale(patterns, lines):
    """Returns the patterns that match no line."""
    return [p for p in patterns if not any(re.search(p, line) for line in lines)]


def self_test():
    lines = [
        "crates/a/src/run.rs:80:5: replace is_executable -> bool with true",
        "crates/a/src/run.rs:75:36: replace && with || in is_executable",
    ]
    ok = [r"run\.rs:80:5: replace is_executable -> bool", r"replace && with \|\| in is_executable"]
    moved = r"run\.rs:79:5: replace is_executable -> bool"
    gone = r"replace removed_fn -> bool with true"
    assert stale(ok, lines) == [], "real entries must pass"
    assert stale(ok + [moved], lines) == [moved], "an entry whose position moved must fail"
    assert stale([gone, *ok], lines) == [gone], "an entry for a deleted function must fail"
    assert stale([moved], []) == [moved], "an empty list matches nothing"
    print("mutants-exclusions self-test ok")


def main(argv):
    if argv == ["--self-test"]:
        self_test()
        return 0
    if len(argv) != 2 or argv[0] != "--list":
        print(__doc__, file=sys.stderr)
        return 2
    lines = (sys.stdin.read() if argv[1] == "-" else Path(argv[1]).read_text(encoding="utf-8")).splitlines()
    if not lines:
        print("mutants-exclusions: the mutant list is empty (did `cargo mutants --list` fail?)", file=sys.stderr)
        return 1
    patterns = tomllib.loads(CONFIG.read_text(encoding="utf-8")).get("exclude_re", [])
    dead = stale(patterns, lines)
    for pattern in dead:
        print(f"mutants-exclusions: matches no mutant: {pattern}", file=sys.stderr)
    if dead:
        print(
            f"mutants-exclusions: {len(dead)} of {len(patterns)} exclude_re entries in {CONFIG} match nothing; "
            "the code they covered moved or is gone. Re-point each at the code, or delete it.",
            file=sys.stderr,
        )
        return 1
    print(f"mutants-exclusions: ok ({len(patterns)} entries)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
