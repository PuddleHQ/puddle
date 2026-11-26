#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Every workflow and action file under .github/ must be valid YAML.

A step name such as `name: rustdoc (-D warnings: a link)` is a mapping inside a mapping value: GitHub
rejects the whole workflow, and only CI notices. With PyYAML installed (the CI runners and the host
lanes have it) every file is parsed for real and the error names file and line. Without it (a
machine with no YAML library) a line check finds the same class of mistake: an unquoted single-line
value that holds `: `, or a tab in the indentation (block scalars such as `run: |` and comments are skipped; a value that starts with `${{` is checked like any other, since a plain scalar may not hold `: ` inside the expression either). The line check cannot see everything a parser sees; the real parse is the gate.

Usage: check-workflow-yaml.py [--line-check] [dir]   (default dir .github; --line-check forces the fallback)
       check-workflow-yaml.py --self-test
"""
import os
import re
import sys
import tempfile

KEY = re.compile(r"^(\s*(?:-\s+)*)([A-Za-z0-9_.\-]+):(?:\s+(.*))?$")


def line_check(path):
    """(line, message) for each mistake the line check can see."""
    errs = []
    block_indent = None  # indentation of the key whose block scalar (| or >) we are inside
    for n, raw in enumerate(open(path, encoding="utf-8").read().split("\n"), 1):
        line = raw.rstrip("\r")
        if not line.strip():
            continue
        indent = len(line) - len(line.lstrip(" "))
        if block_indent is not None:
            if indent > block_indent:
                continue
            block_indent = None
        if line.lstrip().startswith("#"):
            continue
        if "\t" in line[: len(line) - len(line.lstrip())]:
            errs.append((n, "a tab in the indentation"))
            continue
        m = KEY.match(line)
        if not m:
            continue
        prefix, _key, value = m.groups()
        if value is None:
            continue
        value = value.strip()
        if value[:1] in "|>":
            block_indent = len(prefix.replace("-", " "))
            continue
        if value[:1] in "\"'[{&*!" or not value:
            continue
        if value.startswith("#"):
            continue
        if re.search(r":(\s|$)", value.split(" #")[0]):
            errs.append((n, "an unquoted value holds ': ' (YAML reads a mapping inside a mapping value); quote it or drop the colon"))
    return errs


def parse_check(path, yaml):
    try:
        yaml.safe_load(open(path, encoding="utf-8"))
    except yaml.YAMLError as e:
        mark = getattr(e, "problem_mark", None)
        return [((mark.line + 1) if mark else 0, str(getattr(e, "problem", e)))]
    return []


def files_of(root):
    out = []
    for d, _dirs, names in os.walk(root):
        out += [os.path.join(d, f) for f in names if f.endswith((".yml", ".yaml"))]
    return sorted(out)


def run(root, force_line):
    try:
        if force_line:
            raise ImportError
        import yaml
    except ImportError:
        yaml = None
    bad = 0
    files = files_of(root)
    for f in files:
        for n, msg in (parse_check(f, yaml) if yaml else line_check(f)):
            print(f"{f}:{n}: {msg}")
            bad += 1
    if bad:
        return 1
    print(f"workflow-yaml: {len(files)} files ok ({'parsed' if yaml else 'line check, no YAML library'})")
    return 0


def self_test():
    try:
        import yaml  # noqa: F401
        modes = [False, True]
    except ImportError:
        modes = [True]
    with tempfile.TemporaryDirectory() as d:
        wf = os.path.join(d, "workflows")
        os.makedirs(wf)
        good = "name: ci\non: push\njobs:\n  a:\n    steps:\n      - name: fine (ok; really)\n        if: ${{ a && b }}\n        run: |\n          echo 'x: y'\n      - name: \"quoted: colon\"\n        run: echo a\n"
        bad = "name: ci\non: push\njobs:\n  a:\n    steps:\n      - name: a: b\n        run: echo a\n"
        bad_expr = "name: ci\non: push\njobs:\n  a:\n    strategy:\n      matrix:\n        include: ${{ fromJSON('[{\"name\":\"x (planted: y)\"}]') }}\n"
        open(os.path.join(wf, "good.yml"), "w").write(good)
        for force in modes:
            assert run(d, force) == 0, f"a valid workflow failed (line check {force})"
        for label, text in (("name: a: b", bad), ("a ': ' inside a ${{ }} scalar", bad_expr)):
            open(os.path.join(wf, "bad.yml"), "w").write(text)
            for force in modes:
                if run(d, force) != 1:
                    print(f"self-test: {label} passed (line check {force})", file=sys.stderr)
                    return 1
    print("check-workflow-yaml self-test: ok")
    return 0


if __name__ == "__main__":
    args = sys.argv[1:]
    if args == ["--self-test"]:
        sys.exit(self_test())
    force = "--line-check" in args
    args = [a for a in args if a != "--line-check"]
    sys.exit(run(args[0] if args else ".github", force))
