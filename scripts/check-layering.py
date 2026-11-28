#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The crate layering gate: forbidden edges, and the "Depends on" column of 5.2 kept generated.

The architecture pages (docs/architecture/05-building-block-view.md, 5.2) draw the crate graph and
say which edges must not exist. This gate reads `cargo metadata --no-deps` (offline; the manifests
only) and fails on an edge the pages forbid, so a new dependency cannot slip in unseen.

Forbidden edges (production edges: `[dependencies]` and `[build-dependencies]`, any target;
`[dev-dependencies]` are left out)
    - `puddle-proxy` to `puddle-store`, `puddle-inject` or `puddle-logins`: the proxy decides
      through `puddle_types::Policy` and the `Injector` and `ExchangeRewriter` traits, and knows no
      implementation;
    - any product crate except `puddle-compute-msb` to the msb SDK (`microsandbox`,
      `microsandbox-image`, `microsandbox-runtime`): one adapter owns the runtime;
    - a product crate to a test crate (the six test-only crates in TEST_CRATES below);
    - `puddle-agent` to a workspace crate outside AGENT_MAY_USE: the guest binary stays small and
      carries no host code.

The "Depends on" column
    Every table row of 5.2 whose first cell names a workspace crate has its last column generated
    from Cargo: the crate's production dependencies on other workspace crates, without the
    `puddle-` prefix, sorted (a table with no changed cell is left byte for byte as written); `none` for an empty list; "N of the M other product crates" for
    `puddle-host` (HOST_CRATE). The gate fails when a cell differs and prints the fix.

Usage
    scripts/check-layering.py               check the edges and the column
    scripts/check-layering.py --write       rewrite the column in the page
    scripts/check-layering.py --self-test   prove the checks on throwaway data
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PAGE = ROOT / "docs/architecture/05-building-block-view.md"

TEST_CRATES = {
    "puddle-compute-msb-test-support",
    "puddle-e2e",
    "puddle-host-test-support",
    "puddle-proxy-test-support",
    "puddle-test-kit",
    "puddle-vm-tests",
}
NOT_PRODUCT = TEST_CRATES | {"xtask"}
MSB_ADAPTER = "puddle-compute-msb"
MSB_SDK = {"microsandbox", "microsandbox-image", "microsandbox-runtime"}
PROXY_FORBIDS = {"puddle-store", "puddle-inject", "puddle-logins"}
AGENT_MAY_USE = {
    "puddle-agent-proto",
    "puddle-fd-limit",
    "puddle-fs",
    "puddle-ipc",
    "puddle-log",
    "puddle-types",
}
HOST_CRATE = "puddle-host"


def load_graph() -> dict[str, set[str]]:
    """Production dependencies of each workspace crate: package name to dependency names."""
    out = subprocess.run(
        ["cargo", "metadata", "--no-deps", "--offline", "--format-version", "1"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    ).stdout
    return graph_from_metadata(json.loads(out))


def graph_from_metadata(meta: dict) -> dict[str, set[str]]:
    return {
        p["name"]: {d["name"] for d in p["dependencies"] if d["kind"] in (None, "build")}
        for p in meta["packages"]
    }


def edge_errors(graph: dict[str, set[str]]) -> list[str]:
    errors = []
    for crate, deps in sorted(graph.items()):
        if crate == "puddle-proxy":
            for d in sorted(deps & PROXY_FORBIDS):
                errors.append(f"{crate} -> {d}: the proxy knows no store, injector or login implementation")
        if crate != MSB_ADAPTER and crate not in TEST_CRATES:
            for d in sorted(deps & MSB_SDK):
                errors.append(f"{crate} -> {d}: only {MSB_ADAPTER} uses the msb SDK in [dependencies] (a test may use it as a dev-dependency)")
        if crate not in NOT_PRODUCT:
            for d in sorted(deps & TEST_CRATES):
                errors.append(f"{crate} -> {d}: a product crate must not depend on a test crate (use [dev-dependencies])")
        if crate == "puddle-agent":
            for d in sorted(d for d in deps - AGENT_MAY_USE if d in graph):
                errors.append(f"{crate} -> {d}: the guest agent may use only {', '.join(sorted(AGENT_MAY_USE))}")
    return errors


def depends_cell(crate: str, graph: dict[str, set[str]]) -> str:
    deps = sorted(d for d in graph[crate] if d in graph)
    if crate == HOST_CRATE:
        product = {c for c in graph if c not in NOT_PRODUCT and c != crate}
        return f"{len(set(deps) & product)} of the {len(product)} other product crates"
    names = [d.removeprefix("puddle-") if d != "puddle" else d for d in deps]
    return ", ".join(names) if names else "none"


def split_row(line: str) -> list[str]:
    return [c.strip() for c in line.strip().strip("|").split("|")]


def render_table(rows: list[list[str]]) -> list[str]:
    ncol = len(rows[0])
    widths = [max(len(r[i]) for r in rows if r is not None) for i in range(ncol)]
    widths = [max(w, 3) for w in widths]
    out = []
    for r in rows:
        cells = ["-" * widths[i] for i in range(ncol)] if r is None else [r[i].ljust(widths[i]) for i in range(ncol)]
        out.append("| " + " | ".join(cells) + " |")
    return out


def rewrite_page(text: str, graph: dict[str, set[str]]) -> tuple[str, list[str]]:
    """The page with each crate row's last cell generated, and the rows that changed."""
    lines = text.split("\n")
    out, changed, i = [], [], 0
    while i < len(lines):
        if not (lines[i].startswith("| Crate") and i + 1 < len(lines) and set(lines[i + 1]) <= set("|- ")):
            out.append(lines[i])
            i += 1
            continue
        j = i + 2
        while j < len(lines) and lines[j].startswith("|"):
            j += 1
        if split_row(lines[i])[-1] != "Depends on":
            out.extend(lines[i:j])
            i = j
            continue
        before = len(changed)
        rows: list[list[str] | None] = [split_row(lines[i]), None]
        for line in lines[i + 2:j]:
            row = split_row(line)
            name = row[0].strip("`")
            if name in graph:
                new = depends_cell(name, graph)
                if row[-1] != new:
                    changed.append(f"{name}: `{row[-1]}` -> `{new}`")
                    row[-1] = new
            rows.append(row)
        out.extend(render_table(rows) if len(changed) > before else lines[i:j])
        i = j
    return "\n".join(out), changed


def main_check(write: bool) -> int:
    graph = load_graph()
    errors = edge_errors(graph)
    text = PAGE.read_text()
    new_text, changed = rewrite_page(text, graph)
    if write:
        PAGE.write_text(new_text)
        print(f"layering: {len(changed)} cell(s) rewritten in {PAGE.relative_to(ROOT)}")
    elif new_text != text:
        errors += [f"{PAGE.relative_to(ROOT)}, 5.2 Depends on, {c}" for c in changed] or [
            f"{PAGE.relative_to(ROOT)}: table layout differs from the generated one"
        ]
        errors.append("fix: scripts/check-layering.py --write")
    for e in errors:
        print(f"layering: {e}", file=sys.stderr)
    return 1 if errors else 0


def self_test() -> int:
    def meta(deps: dict[str, list[tuple[str, str | None]]]) -> dict:
        return {"packages": [
            {"name": n, "dependencies": [{"name": d, "kind": k} for d, k in ds]} for n, ds in deps.items()
        ]}

    clean = graph_from_metadata(meta({
        "puddle-proxy": [("puddle-types", None), ("puddle-store", "dev")],
        "puddle-compute-msb": [("microsandbox", None)],
        "puddle-store": [("puddle-types", None)],
        "puddle-types": [],
        "puddle-agent": [("puddle-types", None), ("puddle-test-kit", "dev"), ("serde", None)],
        "puddle-test-kit": [("puddle-store", None)],
        "puddle-vm-tests": [("microsandbox", None)],
    }))
    assert edge_errors(clean) == [], edge_errors(clean)

    bad = graph_from_metadata(meta({
        "puddle-proxy": [("puddle-store", None), ("puddle-inject", "build")],
        "puddle-host": [("microsandbox", None), ("puddle-test-kit", None)],
        "puddle-agent": [("puddle-store", None)],
        "puddle-store": [],
        "puddle-ssh": [("microsandbox-image", None)],
    }))
    got = edge_errors(bad)
    for needle in ("puddle-proxy -> puddle-store", "puddle-proxy -> puddle-inject", "puddle-host -> microsandbox:",
                   "puddle-host -> puddle-test-kit", "puddle-agent -> puddle-store", "puddle-ssh -> microsandbox-image"):
        assert any(e.startswith(needle) for e in got), (needle, got)
    assert len(got) == 6, got

    g = {"puddle-a": {"puddle-b", "serde"}, "puddle-b": set(), "puddle": {"puddle-a"}}
    page = "\n".join([
        "text", "| Crate | Responsibility | Depends on |", "| --- | --- | --- |",
        "| `puddle-a` | x | wrong |", "| `puddle-b` | yy | none |", "| `puddle` | z | a |", "tail",
    ])
    new, changed = rewrite_page(page, g)
    assert len(changed) == 1 and changed[0].startswith("puddle-a:"), changed
    assert "| `puddle-a` | x              | b          |" in new, new
    assert rewrite_page(new, g)[1] == [], "second pass must change nothing"
    other = "| Crate | Responsibility |\n| --- | --- |\n| `puddle-a` | x |"
    assert rewrite_page(other, g) == (other, []), "a table without the column is left alone"
    print("layering: self-test ok")
    return 0


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        sys.exit(self_test())
    sys.exit(main_check("--write" in sys.argv))
