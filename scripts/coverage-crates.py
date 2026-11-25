#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The crates the coverage totals count (docs/STANDARDS.md, "Coverage").

Usage:
  coverage-crates.py [--metadata FILE]   check scripts/coverage-crates.txt against the workspace; print
                                         the --ignore-filename-regex that leaves the test-only crates out
  coverage-crates.py --self-test

The product crates are the dependency closure (normal and build dependencies) of the `ship` binaries in
scripts/coverage-crates.txt, taken from `cargo metadata --all-features`. Fails when a workspace crate is neither
in that closure nor listed `test-only`, when a listed name is not a workspace crate, and when a `test-only`
crate is in the closure of a shipped binary.
"""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
LIST = os.path.join(HERE, "coverage-crates.txt")


def read_list(path):
    ship, test_only = [], []
    for n, raw in enumerate(open(path, encoding="utf-8"), 1):
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        parts = line.split()
        if len(parts) != 2 or parts[0] not in ("ship", "test-only"):
            raise SystemExit(f"{path}:{n}: expected `ship <crate>` or `test-only <crate>`, got: {raw.strip()}")
        (ship if parts[0] == "ship" else test_only).append(parts[1])
    return ship, test_only


def closure(meta, roots):
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    seen, stack = set(), list(roots)
    while stack:
        i = stack.pop()
        if i in seen:
            continue
        seen.add(i)
        for d in nodes[i]["deps"]:
            if any(k["kind"] in (None, "build") for k in d["dep_kinds"]):
                stack.append(d["pkg"])
    return seen


def judge(meta, ship, test_only):
    """Returns (problems, test-only crate names)."""
    by_id = {p["id"]: p["name"] for p in meta["packages"]}
    ws = {i: by_id[i] for i in meta["workspace_members"]}
    names = set(ws.values())
    problems = [f"`{c}` in {os.path.basename(LIST)} is not a workspace crate" for c in ship + test_only if c not in names]
    roots = [i for i, n in ws.items() if n in ship]
    inside = {by_id[i] for i in closure(meta, roots)} & names
    problems += [f"`{c}` is listed test-only but a shipped binary depends on it" for c in sorted(set(test_only) & inside)]
    problems += [
        f"workspace crate `{c}` is neither in the dependency closure of the shipped binaries nor listed test-only"
        for c in sorted(names - inside - set(test_only))
    ]
    return problems, sorted(set(test_only) & names)


def regex(crates):
    return "/crates/(" + "|".join(crates) + ")/"


def meta_of(path):
    if path:
        return json.load(open(path, encoding="utf-8"))
    out = subprocess.run(
        [os.environ.get("CARGO", "cargo"), "metadata", "--format-version", "1", "--locked", "--all-features"],
        check=True, capture_output=True, text=True,
    ).stdout
    return json.loads(out)


def pkg(name, deps=()):
    return {"id": name, "name": name, "deps": list(deps)}


def fake(packages):
    return {
        "packages": [{"id": p["id"], "name": p["name"]} for p in packages],
        "workspace_members": [p["id"] for p in packages],
        "resolve": {"nodes": [
            {"id": p["id"], "deps": [{"pkg": d, "dep_kinds": [{"kind": k}]} for d, k in p["deps"]]} for p in packages
        ]},
    }


def self_test():
    ok = fake([pkg("bin", [("lib", None), ("tool", "build"), ("kit", "dev")]), pkg("lib"), pkg("tool"), pkg("kit"), pkg("e2e", [("lib", None)])])
    problems, t = judge(ok, ["bin"], ["kit", "e2e"])
    assert problems == [] and t == ["e2e", "kit"], (problems, t)
    problems, _ = judge(ok, ["bin"], ["kit"])
    assert any("`e2e` is neither" in p for p in problems), problems
    assert not any("dev" in p or "`kit`" in p for p in problems), problems
    problems, _ = judge(ok, ["bin"], ["kit", "e2e", "lib"])
    assert any("`lib` is listed test-only but" in p for p in problems), problems
    problems, _ = judge(ok, ["bin"], ["kit", "e2e", "ghost"])
    assert any("`ghost` in" in p and "not a workspace crate" in p for p in problems), problems
    assert regex(["a", "b"]) == "/crates/(a|b)/"
    print("coverage-crates self-test ok", file=sys.stderr)


def main(argv):
    if argv[1:] == ["--self-test"]:
        return self_test()
    path = argv[2] if argv[1:2] == ["--metadata"] else None
    ship, test_only = read_list(LIST)
    problems, excluded = judge(meta_of(path), ship, test_only)
    if problems:
        for p in problems:
            print(f"coverage-crates: {p}", file=sys.stderr)
        print("coverage-crates: add the crate to scripts/coverage-crates.txt (`ship` for a binary, `test-only` for a harness) or make a shipped binary depend on it", file=sys.stderr)
        raise SystemExit(1)
    print(regex(excluded))


if __name__ == "__main__":
    main(sys.argv)
