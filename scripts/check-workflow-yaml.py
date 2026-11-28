#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Every workflow and action file under .github/ must be valid YAML.

A step name such as `name: rustdoc (-D warnings: a link)` is a mapping inside a mapping value: GitHub
rejects the whole workflow, and only CI notices. With PyYAML installed (the CI runners and the host
lanes have it) every file is parsed for real and the error names file and line. Without it (a
machine with no YAML library) a line check finds the same class of mistake: an unquoted single-line
value that holds `: `, or a tab in the indentation (block scalars such as `run: |` and comments are skipped; a value that starts with `${{` is checked like any other, since a plain scalar may not hold `: ` inside the expression either). Both paths also reject a repeated key in one mapping (GitHub refuses the workflow; PyYAML would keep the last value silently). The line check cannot see everything a parser sees; the real parse is the gate.

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
    seen = {}  # mapping indentation -> keys seen so far in the mapping open at that indentation
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
        prefix, key, value = m.groups()
        depth = len(prefix.replace("-", " "))
        for d in [d for d in seen if d > depth]:
            del seen[d]
        if "-" in prefix or depth not in seen:
            seen[depth] = set()
        if key in seen[depth]:
            errs.append((n, f"duplicate key '{key}' in one mapping"))
        seen[depth].add(key)
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


def unique_key_loader(yaml):
    """A SafeLoader that raises on a repeated key in one mapping instead of keeping the last."""

    class Loader(yaml.SafeLoader):
        def construct_mapping(self, node, deep=False):
            seen = set()
            for key_node, _value in node.value:
                if key_node.tag == "tag:yaml.org,2002:merge":
                    continue
                key = self.construct_object(key_node, deep=deep)
                if isinstance(key, (str, int, float, bool)) and key in seen:
                    raise yaml.constructor.ConstructorError(
                        "while constructing a mapping", node.start_mark, f"duplicate key '{key}'", key_node.start_mark
                    )
                seen.add(key)
            return super().construct_mapping(node, deep)

    return Loader


def parse_check(path, yaml):
    try:
        yaml.load(open(path, encoding="utf-8"), Loader=unique_key_loader(yaml))
    except yaml.YAMLError as e:
        mark = getattr(e, "problem_mark", None) or getattr(e, "context_mark", None)
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
        dup_job = "name: ci\non: push\njobs:\n  a:\n    env:\n      X: '1'\n    runs-on: x\n    env:\n      Y: '2'\n"
        dup_step = "name: ci\non: push\njobs:\n  a:\n    steps:\n      - name: one\n        run: a\n        run: b\n"
        same_key_in_two_items = "name: ci\non: push\njobs:\n  a:\n    env:\n      X: '1'\n  b:\n    env:\n      X: '1'\n    steps:\n      - run: a\n      - run: b\n"
        open(os.path.join(wf, "good.yml"), "w").write(good)
        for force in modes:
            assert run(d, force) == 0, f"a valid workflow failed (line check {force})"
        open(os.path.join(wf, "good.yml"), "w").write(same_key_in_two_items)
        for force in modes:
            assert run(d, force) == 0, f"the same key in two mappings failed (line check {force})"
        open(os.path.join(wf, "good.yml"), "w").write(good)
        for label, text in (
            ("name: a: b", bad),
            ("a ': ' inside a ${{ }} scalar", bad_expr),
            ("a duplicate job key", dup_job),
            ("a duplicate step key", dup_step),
        ):
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
