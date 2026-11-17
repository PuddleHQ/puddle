#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Summarise the zero-on-free A/B of the proxy perf tests (the `zero-on-free-ab` job of perf.yml).

    perf-zero-ab-summary.py <dir>

<dir> holds one log per run, named `<side>-<n>.log` (side `off` or `on`), each the output of the
perf tests with PUDDLE_PERF_ZERO_ON_FREE set accordingly. A log carries lines

    A zero_on_free on|off
    M <label> wall <seconds> user <s> system <s>      (or: M <label> wall <seconds> cpu unknown)
    M latency median_us <n> p99_us <n> requests <n>

Per run and label the best (lowest) figure counts, as the tests themselves keep the faster of
their repeats; the table shows the median of those over the runs of one side, then on over off.
Prints a markdown table (and to $GITHUB_STEP_SUMMARY when set). Exit 1 when a log is missing,
says the other side than its name, or a side has no measurement.
"""

import os
import re
import statistics
import sys
from collections import defaultdict
from pathlib import Path

WALL = re.compile(r"(?:^|\s)M (?P<label>.+?) wall (?P<wall>[0-9.]+)(?: user (?P<user>[0-9.]+) system (?P<sys>[0-9.]+))?")
LATENCY = re.compile(r"(?:^|\s)M latency median_us (?P<median>\d+) p99_us (?P<p99>\d+)")
SIDE = re.compile(r"(?:^|\s)A zero_on_free (on|off)$")


def parse(path: Path, side: str) -> dict[str, float]:
    """Best figure per metric in one log."""
    best: dict[str, float] = {}
    said = None
    for line in path.read_text(errors="replace").splitlines():
        if m := SIDE.search(line):
            said = m.group(1)
            if said != side:
                raise SystemExit(f"{path}: the run says zero_on_free {said}, its name says {side}")
            continue
        found: dict[str, float] = {}
        if m := WALL.search(line):
            found[f"{m['label']} wall s"] = float(m["wall"])
            if m["user"] is not None:
                found[f"{m['label']} cpu s"] = float(m["user"]) + float(m["sys"])
        elif m := LATENCY.search(line):
            found["latency median us"] = float(m["median"])
            found["latency p99 us"] = float(m["p99"])
        for key, value in found.items():
            best[key] = min(value, best.get(key, value))
    if said is None:
        raise SystemExit(f"{path}: no `A zero_on_free` line, the run did not start")
    return best


def main(directory: str) -> int:
    runs: dict[str, dict[str, list[float]]] = {"off": defaultdict(list), "on": defaultdict(list)}
    counts = {"off": 0, "on": 0}
    for side in runs:
        for path in sorted(Path(directory).glob(f"{side}-*.log")):
            counts[side] += 1
            for key, value in parse(path, side).items():
                runs[side][key].append(value)
    if not counts["off"] or counts["off"] != counts["on"]:
        print(f"runs per side differ or are missing: {counts}", file=sys.stderr)
        return 1
    rows = ["| metric | off (median of %d) | on (median of %d) | on / off | off range | on range |" % (counts["off"], counts["on"]), "|---|---|---|---|---|---|"]
    for key in sorted(runs["off"]):
        off, on = runs["off"][key], runs["on"].get(key, [])
        if len(on) != len(off):
            print(f"{key}: {len(off)} off runs, {len(on)} on runs", file=sys.stderr)
            return 1
        m_off, m_on = statistics.median(off), statistics.median(on)
        rows.append(
            f"| {key} | {m_off:.3f} | {m_on:.3f} | {m_on / m_off:.3f} | {min(off):.3f} to {max(off):.3f} | {min(on):.3f} to {max(on):.3f} |"
        )
    text = "\n".join(rows)
    print(text)
    if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a", encoding="utf-8") as out:
            out.write(f"### zero-on-free A/B ({os.environ.get('RUNNER_OS', 'local')})\n\n{text}\n")
    return 0


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit(__doc__)
    sys.exit(main(sys.argv[1]))
