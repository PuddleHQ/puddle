#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Fails an `exclude_re` entry of .cargo/mutants.toml that matches no mutant.

An exclusion names a mutant by its `cargo mutants --list` line (`file:line:column: description`). It keys
on the function name and the mutation text, never on a literal line: an edit above the entry would move
the line and fail an unrelated change. Where two mutants read the same (the Windows and the Unix body of
one function, three equal comparisons in one function), the code gives them different function names.
This gate fails an entry that names a literal line, and (listing the mutants without the config) every
entry that matches none of them, so a deleted or renamed function sends the author back to the entry.

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


# `file.rs:` followed by a digit or a group of digits is a line number.
LINE_KEYED = re.compile(r"\.rs:\(?[0-9]")


def line_keyed(patterns):
    """Returns the patterns that name a literal line number, which breaks when code above it moves."""
    return [p for p in patterns if LINE_KEYED.search(p)]


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
    on, off = "\x1b[38;5;13m", "\x1b[0m"
    coloured = ["crates/a/src/run.rs:80:5: replace " + on + "is_executable" + off + " -> bool with true"]
    assert stale(ok[:1], coloured) == ok[:1], "colour escapes split the line, so the entry misses"
    assert line_keyed([r"run\.rs:98:5: replace is_executable", r"records\.rs:(124|153):\d+: replace"]) != [], "literal lines fail"
    assert line_keyed([r"run\.rs:98:5: x"]) == [r"run\.rs:98:5: x"], "a literal line number is caught"
    assert line_keyed([r"session\.rs:\d+:8: delete ! in send_and_pump", "replace ask_pipe "]) == [], "text keys pass"
    shifted = ["crates/a/src/run.rs:95:5: replace exists_as_file -> bool with true"]
    assert stale(["replace exists_as_file "], lines + shifted) == [], "a text key survives a moved line"
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
    if any("\x1b" in line for line in lines):
        print(
            "mutants-exclusions: the mutant list holds colour escapes, so no entry can match; "
            "list with `cargo mutants --list --no-config --colors never`",
            file=sys.stderr,
        )
        return 1
    patterns = tomllib.loads(CONFIG.read_text(encoding="utf-8")).get("exclude_re", [])
    dead = stale(patterns, lines)
    pinned = line_keyed(patterns)
    for pattern in pinned:
        print(f"mutants-exclusions: names a literal line number: {pattern}", file=sys.stderr)
    if pinned:
        print(
            "mutants-exclusions: key an entry on the function name and the mutation text, not on a line: "
            "give a cfg twin or a lookalike its own function name instead (STANDARDS.md, Mutation testing).",
            file=sys.stderr,
        )
        return 1
    for pattern in dead:
        print(f"mutants-exclusions: matches no mutant: {pattern}", file=sys.stderr)
    if dead:
        print(f"mutants-exclusions: the list had {len(lines)} lines; first: {lines[0]}", file=sys.stderr)
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
