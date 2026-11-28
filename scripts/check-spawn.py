#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Host code starts every child process through `puddle_process::command`.

The desktop app is a Windows GUI program and has no console. Windows gives each console program
such a process starts a console of its own, and that console's window shows on the user's desktop
for as long as the program runs: a `git --version` or `msb --version` flashes a terminal window.
`puddle_process::command(program)` sets `CREATE_NO_WINDOW`, which gives the child a console that
has no window. This gate fails every `Command::new` of `std::process` or `tokio::process` in the
production code of the host-side crates, so a new spawn site cannot forget the flag.

The fix it names
    std:    puddle_process::command(program)
    tokio:  tokio::process::Command::from(puddle_process::command(program))
A spawn that really must not go through the helper takes a comment `spawn-ok: <reason>` on the line
or one of the 3 lines above it. The reason is required.

What it scans
    crates/<crate>/src/**/*.rs, except the crates in EXEMPT below (each says why), files named
    tests.rs or testing.rs, files under a tests/ folder, and inline `#[cfg(test)] mod ... { }` blocks.
    A `Command` counts as a process command when it is written with its path (`std::process::Command`,
    `tokio::process::Command`) or brought in by a `use` of one of those: a use tree at any depth
    (`use std::{io, process::Command}`), an `as` alias, a glob, or the module itself
    (`use std::process; process::Command::new`, `use std::process as p`).

What it cannot see
    - a process started another way: `CreateProcessW` called directly, `Command::from` of a command
      made elsewhere, a crate that starts processes for us (the microsandbox SDK starts msb itself);
    - a caller that overwrites the flags with its own `creation_flags(...)` after the helper;
    - braces inside strings or char literals in a test block (the block end can drift).

Usage
    scripts/check-spawn.py               scan the repository
    scripts/check-spawn.py --self-test   prove the scan on throwaway files
"""

from __future__ import annotations

import re
import sys
import tempfile
from pathlib import Path

EXEMPT = {
    "puddle-agent": "runs inside the Linux guest, which has no console windows",
    "puddle-process": "the helper itself",
    "xtask": "a developer tool run from a terminal",
    "puddle-e2e": "a test harness",
    "puddle-vm-tests": "a test harness",
    "puddle-test-kit": "a test harness",
    "puddle-proxy-test-support": "a test harness",
    "puddle-host-test-support": "a test harness",
    "puddle-compute-msb-test-support": "a test harness",
}

FIX = (
    "use puddle_process::command(program) "
    "(tokio: tokio::process::Command::from(puddle_process::command(program))), "
    "so Windows opens no console window for it; or a `spawn-ok: <reason>` comment"
)

PATHED = re.compile(r"\b(?:std|tokio)::process::Command\s*::\s*new\b")
USE_STMT = re.compile(r"(?:^|[;{}\]])\s*(?:pub(?:\([^)]*\))?\s+)?use\s+([\w:\s,{}*]*);", re.M)
CFG_TEST = re.compile(r"^\s*#\[cfg\((?:test|all\(test\b|any\(test\b)")
MOD_OPEN = re.compile(r"^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+\w+\s*\{")
MARKER = re.compile(r"spawn-ok:")
MARKER_REASON = re.compile(r"spawn-ok:\s*\S")


def scanned(path: Path) -> bool:
    if path.name in ("tests.rs", "testing.rs"):
        return False
    return "tests" not in path.parts[path.parts.index("src") :]


def split_top(text: str) -> list[str]:
    """Splits a use-tree body at the commas outside braces."""
    items, depth, start = [], 0, 0
    for i, ch in enumerate(text):
        depth += ch == "{"
        depth -= ch == "}"
        if ch == "," and depth == 0:
            items.append(text[start:i])
            start = i + 1
    items.append(text[start:])
    return [i for i in items if i.strip()]


def expand(prefix: list[str], tree: str):
    """Yields (path, alias) for every leaf of a use tree; `self` is the prefix itself, `*` a glob."""
    tree = tree.strip()
    if "{" in tree:
        head, rest = tree.split("{", 1)
        head_path = prefix + [seg.strip() for seg in head.split("::") if seg.strip()]
        for item in split_top(rest[: rest.rindex("}")]):
            yield from expand(head_path, item)
        return
    parts = tree.split()
    path = prefix + [seg for seg in parts[0].split("::") if seg]
    if path[-1] == "self" and len(path) > 1:
        path = path[:-1]
    yield path, parts[2] if len(parts) == 3 and parts[1] == "as" else None


def process_names(text: str) -> tuple[set[str], set[str]]:
    """The names `Command` and the std/tokio `process` module go by in this file."""
    commands: set[str] = set()
    modules: set[str] = set()
    code = "\n".join(line.split("//", 1)[0] for line in text.splitlines())
    for m in USE_STMT.finditer(code):
        if m.group(1).count("{") != m.group(1).count("}"):
            continue  # prose that starts with the word `use`, not a use tree
        for path, alias in expand([], m.group(1)):
            if path[0] not in ("std", "tokio") or len(path) < 2 or path[1] != "process":
                continue
            if len(path) == 2:
                modules.add(alias or "process")
            elif len(path) == 3 and path[2] == "*":
                commands.add("Command")
            elif len(path) == 3 and path[2] == "Command":
                commands.add(alias or "Command")
    commands.discard("_")
    return commands, modules


def production_lines(lines: list[str]):
    """Yields (number, line) for the lines outside inline test modules."""
    skip_depth = None
    pending_cfg = False
    for n, line in enumerate(lines, 1):
        if skip_depth is not None:
            skip_depth += line.count("{") - line.count("}")
            if skip_depth <= 0:
                skip_depth = None
            continue
        if CFG_TEST.match(line):
            pending_cfg = True
            continue
        if pending_cfg and line.strip().startswith("#["):
            continue
        if pending_cfg and MOD_OPEN.match(line):
            pending_cfg = False
            skip_depth = line.count("{") - line.count("}")
            if skip_depth <= 0:
                skip_depth = None
            continue
        pending_cfg = False
        yield n, line


def scan_file(path: Path, shown: str) -> list[str]:
    text = path.read_text(encoding="utf-8", errors="replace")
    lines = text.splitlines()
    commands, modules = process_names(text)
    starts = [re.escape(n) for n in sorted(commands)] + [re.escape(m) + r"\s*::\s*Command" for m in sorted(modules)]
    local = re.compile(r"(?<![\w:])(?:" + "|".join(starts) + r")\s*::\s*new\b") if starts else None
    problems = []
    for n, line in production_lines(lines):
        code = line.split("//", 1)[0]
        if not (PATHED.search(code) or (local and local.search(code))):
            continue
        window = lines[max(0, n - 4) : n]
        if any(MARKER_REASON.search(w) for w in window):
            continue
        if any(MARKER.search(w) for w in window):
            problems.append(f"{shown}:{n}: `spawn-ok:` without a reason")
            continue
        problems.append(f"{shown}:{n}: starts a process with Command::new; {FIX}")
    return problems


def scan(root: Path) -> list[str]:
    problems = []
    for crate in sorted((root / "crates").iterdir()):
        if crate.name in EXEMPT or not (crate / "src").is_dir():
            continue
        for path in sorted((crate / "src").rglob("*.rs")):
            if scanned(path):
                problems += scan_file(path, str(path.relative_to(root)))
    return problems


SELF_TEST = {
    # (file, content, expected number of problems)
    "crates/a/src/pathed.rs": ("fn f() { std::process::Command::new(\"git\"); }\n", 1),
    "crates/a/src/tokio_alias.rs": (
        "use tokio::process::Command as Tk;\nfn f() { Tk::new(\"az\"); }\n",
        1,
    ),
    "crates/a/src/group.rs": (
        "use std::process::{\n    Child,\n    Command,\n    Stdio,\n};\nfn f() { let c = Command::new(\"msb\"); }\n",
        1,
    ),
    "crates/a/src/nested_use.rs": ("use std::{io, process::Command};\nfn f() { Command::new(\"git\"); }\n", 1),
    "crates/a/src/module_use.rs": ("use std::process;\nfn f() { process::Command::new(\"git\"); }\n", 1),
    "crates/a/src/module_alias.rs": ("use std::process as p;\nfn f() { p::Command::new(\"git\"); }\n", 1),
    "crates/a/src/self_group.rs": (
        "use std::process::{self, Command as C};\nfn f() {\n    process::Command::new(\"a\");\n    C::new(\"b\");\n}\n",
        2,
    ),
    "crates/a/src/tokio_nested.rs": (
        "use tokio::{fs, process::{Child, Command}};\nfn f() { Command::new(\"az\"); }\n",
        1,
    ),
    "crates/a/src/glob.rs": ("use std::process::*;\nfn f() { Command::new(\"git\"); }\n", 1),
    "crates/a/src/leading_colons.rs": ("fn f() { ::std::process::Command::new(\"git\"); }\n", 1),
    "crates/a/src/clap_nested.rs": ("use clap::{Arg, Command};\nfn f() { Command::new(\"puddle\"); }\n", 0),
    "crates/a/src/own_process_module.rs": ("use crate::process;\nfn f() { process::Command::new(\"x\"); }\n", 0),
    "crates/a/src/nested_marker.rs": (
        "use std::{io, process::Command};\nfn f() {\n    // spawn-ok: needs a console\n    Command::new(\"cmd\");\n}\n",
        0,
    ),
    "crates/a/src/prose_use.rs": (
        "const S: &str = \"a\nuse the {x\nand more\";\nfn f() { Command::new(\"x\"); }\n",
        0,
    ),
    "crates/a/src/clap.rs": ("use clap::Command;\nfn f() { Command::new(\"puddle\"); }\n", 0),
    "crates/a/src/helper.rs": ("fn f() { puddle_process::command(\"git\"); }\n", 0),
    "crates/a/src/marker.rs": (
        "fn f() {\n    // spawn-ok: the child must have a visible console\n    std::process::Command::new(\"cmd\");\n}\n",
        0,
    ),
    "crates/a/src/bare_marker.rs": ("fn f() {\n    // spawn-ok:\n    std::process::Command::new(\"cmd\");\n}\n", 1),
    "crates/a/src/comment.rs": ("// a std::process::Command::new(\"x\") in prose\nfn f() {}\n", 0),
    "crates/a/src/inline_tests.rs": (
        "use std::process::Command;\nfn f() {}\n#[cfg(test)]\n#[expect(x)]\nmod tests {\n    fn t() {\n        Command::new(\"sh\");\n    }\n}\nfn g() { Command::new(\"after\"); }\n",
        1,
    ),
    "crates/a/src/tests.rs": ("fn t() { std::process::Command::new(\"sh\"); }\n", 0),
    "crates/a/src/x/tests/helper.rs": ("fn t() { std::process::Command::new(\"sh\"); }\n", 0),
    "crates/a/src/testing.rs": ("fn t() { std::process::Command::new(\"sh\"); }\n", 0),
    "crates/a/tests/integration.rs": ("fn t() { std::process::Command::new(\"sh\"); }\n", 0),
    "crates/puddle-agent/src/lib.rs": ("fn f() { std::process::Command::new(\"sh\"); }\n", 0),
}


def self_test() -> None:
    with tempfile.TemporaryDirectory(prefix="check-spawn.") as tmp:
        root = Path(tmp)
        for name, (content, _) in SELF_TEST.items():
            path = root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        found = scan(root)
        failures = []
        for name, (_, want) in SELF_TEST.items():
            got = sum(1 for p in found if p.startswith(name + ":"))
            if got != want:
                failures.append(f"{name}: {got} problems, want {want}")
        if not any("inline_tests.rs:10:" in p for p in found):
            failures.append("inline_tests.rs: the spawn after the test module was not flagged on line 10")
        if failures:
            print("check-spawn self-test failed:\n  " + "\n  ".join(failures) + "\nfound:\n  " + "\n  ".join(found), file=sys.stderr)
            sys.exit(1)
    print("check-spawn self-test: ok")


def main() -> None:
    if sys.argv[1:] == ["--self-test"]:
        self_test()
        return
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent
    problems = scan(root)
    for p in problems:
        print(p, file=sys.stderr)
    if problems:
        print(f"spawn: {len(problems)} process start(s) can open a console window on Windows", file=sys.stderr)
        sys.exit(1)
    print("spawn: every host process start goes through puddle_process::command")


if __name__ == "__main__":
    main()
