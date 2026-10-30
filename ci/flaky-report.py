#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""List the tests that failed and then passed on a retry, from a nextest junit.xml.

Usage: flaky-report.py <junit.xml>. Prints a Markdown section (appended to the job summary by the
caller), emits one ::warning:: per flaky test, and always exits 0: a flake is reported, never red.
Genuine failures (no pass on the retry) stay the test step's own failure.

Stress mode, for the nightly stress job (ci/stress.sh): flaky-report.py --stress <rust|vitest|playwright>
<report>. Every test that failed in any repeat is listed with how many of its runs failed (a test that
failed once in 20 is the finding, there is no retry): a Markdown section and one ::warning:: per test.
ci/stress-yield.py reads the same counts through junit_counts and playwright_counts. The exit status is
0: the stress step's own status is the job's.
"""
import json
import sys
import xml.etree.ElementTree as ET


def junit_counts(path: str) -> dict[str, list[int]]:
    """nextest and vitest junit: name -> [failed, runs] over every repeat in the file."""
    counts: dict[str, list[int]] = {}
    for case in ET.parse(path).getroot().iter("testcase"):
        name = f"{case.get('classname', '')}::{case.get('name', '')}"
        failed = case.find("failure") is not None or case.find("error") is not None
        entry = counts.setdefault(name, [0, 0])
        entry[0] += failed
        entry[1] += 1
    return counts


def playwright_counts(path: str) -> dict[str, list[int]]:
    """Playwright's json reporter: `file > title [project]` -> [failed, runs] over every repeat."""
    counts: dict[str, list[int]] = {}

    def walk(suite: dict) -> None:
        for spec in suite.get("specs", []):
            for test in spec.get("tests", []):
                name = f"{spec.get('file', '')} > {spec.get('title', '')} [{test.get('projectName', '')}]"
                for result in test.get("results", []):
                    entry = counts.setdefault(name, [0, 0])
                    entry[0] += result.get("status") not in ("passed", "skipped")
                    entry[1] += 1
        for child in suite.get("suites", []):
            walk(child)

    with open(path, encoding="utf-8") as report:
        for suite in json.load(report).get("suites", []):
            walk(suite)
    return counts


def stress_main(kind: str, path: str) -> int:
    try:
        counts = playwright_counts(path) if kind == "playwright" else junit_counts(path)
    except (OSError, ET.ParseError, ValueError) as err:
        # No report is the caller's finding (the run never reached its tests), so nothing is listed.
        print(f"### Stress ({kind})\n\nNo report: {err}")
        return 0
    failing = {name: pair for name, pair in sorted(counts.items()) if pair[0]}
    runs = sum(pair[1] for pair in counts.values())
    print(f"### Stress ({kind}): {len(counts)} tests, {runs} runs\n")
    if not failing:
        print("No failures.")
    for name, (failed, total) in failing.items():
        print(f"- `{name}`: failed {failed} of {total} runs")
        print(f"::warning title=stress failure::{name} failed {failed} of {total} runs", file=sys.stderr)
    return 0


def main() -> int:
    if sys.argv[1:2] == ["--stress"]:
        if len(sys.argv) != 4:
            print("usage: flaky-report.py --stress <rust|vitest|playwright> <report>", file=sys.stderr)
            return 2
        return stress_main(sys.argv[2], sys.argv[3])
    try:
        root = ET.parse(sys.argv[1]).getroot()
    except (OSError, ET.ParseError, IndexError) as err:
        print(f"### Flaky tests\n\nNo report: {err}")
        return 0
    flaky = []
    for case in root.iter("testcase"):
        if case.find("flakyFailure") is not None or case.find("flakyError") is not None:
            flaky.append(f"{case.get('classname', '')}::{case.get('name', '')}")
    print("### Flaky tests (failed, passed on one retry)\n")
    if not flaky:
        print("None.")
    for name in sorted(flaky):
        print(f"- `{name}`")
        print(f"::warning title=flaky test::{name} failed and passed on retry", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
