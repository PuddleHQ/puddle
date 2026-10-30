#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""The nightly stress job's yield record: what each night found, and what it cost.

  stress-yield.py record <out-dir> <kind> --sha S --run-id N --job-seconds N [--date D]
      Reads <out-dir>/<kind>/result.env (written by ci/stress.sh), lists the failing tests through
      `flaky-report.py --stress`, matches them against ci/known-flakes.tsv (flakes already fixed, which
      the stress must be able to see on the commit before the fix), and appends one row to
      <out-dir>/yield.tsv. Prints the row as Markdown for the job summary.
  stress-yield.py report <yield.tsv>...
      Adds up rows (one file per night, as the nightly uploads them) into the numbers the yield
      decision needs. `report --download N` first fetches the last N runs' `stress-yield-*` artifacts
      with gh into a temporary directory.

One row per job per night (rust runs as two partitions, so two rows). Columns: date, sha, run, kind, status (ok, failed, no-report), wall_s (the
tool), runner_min (the whole job, setup and build included), failures (tests that failed in any
repeat), known (of those, ones in known-flakes.tsv), tests (`name:failed/runs`, `;` separated), runs (test runs in the report), args (what the
tool ran with: counts, CPUs, the vitest seed, so a failure can be run again).
"""
import csv
import datetime
import importlib.util
import os
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
COLUMNS = ["date", "sha", "run", "kind", "status", "wall_s", "runner_min", "failures", "known", "tests", "args", "runs"]


def load_parsers():
    """flaky-report.py has a hyphen in its name, so it is loaded by path."""
    spec = importlib.util.spec_from_file_location("flaky_report", os.path.join(HERE, "flaky-report.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def known_flakes(path: str) -> list[tuple[str, str]]:
    """`substring<TAB>fix` per line; '#' lines and blanks are comments."""
    entries = []
    with open(path, encoding="utf-8") as lines:
        for line in lines:
            line = line.rstrip("\n")
            if line and not line.startswith("#"):
                needle, _, fix = line.partition("\t")
                entries.append((needle, fix))
    return entries


def read_env(path: str) -> dict[str, str]:
    values = {}
    with open(path, encoding="utf-8") as lines:
        for line in lines:
            key, _, value = line.rstrip("\n").partition("=")
            values[key] = value
    return values


def record(out_dir: str, kind: str, sha: str, run: str, job_seconds: int, date: str, known_path: str) -> dict[str, str]:
    result_path = os.path.join(out_dir, kind, "result.env")
    tests, status, wall, args, runs = [], "no-report", 0, "", 0
    if os.path.exists(result_path):
        result = read_env(result_path)
        wall = int(result.get("WALL_S", "0"))
        args = result.get("ARGS", "")
        report = os.path.join(out_dir, kind, result.get("REPORT", ""))
        if os.path.isfile(report):
            parsers = load_parsers()
            try:
                counts = parsers.playwright_counts(report) if kind == "playwright" else parsers.junit_counts(report)
            except Exception:  # an unreadable report is the same finding as a missing one
                counts = {}
            runs = sum(pair[1] for pair in counts.values())
            tests = [[name, str(pair[0]), str(pair[1])] for name, pair in sorted(counts.items()) if pair[0]]
            # A report with no test runs in it means the tool ran nothing: never a green night.
            status = "no-report" if runs == 0 else "failed" if tests else "ok"
            if result.get("EXIT", "0") != "0" and not tests:
                # The tool failed without a failing test: it never reached them (build, setup, timeout).
                status = "no-report"
    needles = [needle for needle, _ in known_flakes(known_path)]
    known = sum(any(needle in name for needle in needles) for name, _, _ in tests)
    return {
        "date": date, "sha": sha, "run": run, "kind": kind, "status": status, "wall_s": str(wall),
        "runner_min": f"{job_seconds / 60:.1f}", "failures": str(len(tests)), "known": str(known),
        "tests": ";".join(f"{name}:{failed}/{runs}" for name, failed, runs in tests), "args": args, "runs": str(runs),
    }


def append_row(path: str, row: dict[str, str]) -> None:
    fresh = not os.path.exists(path)
    with open(path, "a", encoding="utf-8", newline="") as handle:
        writer = csv.DictWriter(handle, fieldnames=COLUMNS, delimiter="\t", lineterminator="\n")
        if fresh:
            writer.writeheader()
        writer.writerow(row)


def read_rows(paths: list[str]) -> list[dict[str, str]]:
    rows = []
    for path in paths:
        with open(path, encoding="utf-8", newline="") as handle:
            rows.extend(csv.DictReader(handle, delimiter="\t"))
    return rows


def report(rows: list[dict[str, str]]) -> str:
    if not rows:
        return "No stress rows."
    nights = sorted({row["date"] for row in rows})
    lines = [
        f"{len(nights)} nights ({nights[0]} to {nights[-1]}), {len(rows)} rows",
        "",
        "| kind | nights | jobs | red | no report | failing tests | known fixed | new | wall min | runner-min |",
        "|---|---|---|---|---|---|---|---|---|---|",
    ]
    for kind in sorted({row["kind"] for row in rows}):
        mine = [row for row in rows if row["kind"] == kind]
        failures = sum(int(row["failures"]) for row in mine)
        known = sum(int(row["known"]) for row in mine)
        lines.append(
            f"| {kind} | {len({row['date'] for row in mine})} | {len(mine)} | {sum(row['status'] == 'failed' for row in mine)} "
            f"| {sum(row['status'] == 'no-report' for row in mine)} | {failures} | {known} | {failures - known} "
            f"| {sum(int(row['wall_s']) for row in mine) / 60:.1f} | {sum(float(row['runner_min']) for row in mine):.1f} |"
        )
    distinct = {name.rsplit(":", 1)[0] for row in rows for name in row["tests"].split(";") if name}
    runner_min = sum(float(row["runner_min"]) for row in rows)
    lines += [
        "",
        f"{len(distinct)} distinct failing tests over {runner_min:.0f} runner-min "
        f"({runner_min / max(len(nights), 1):.0f} per night). New flakes are the `new` column.",
    ]
    return "\n".join(lines)


def download(count: int, directory: str) -> list[str]:
    runs = subprocess.run(
        ["gh", "run", "list", "--workflow", "stress.yml", "--limit", str(count), "--json", "databaseId", "--jq", ".[].databaseId"],
        check=True, capture_output=True, text=True,
    ).stdout.split()
    found = []
    for run in runs:
        target = os.path.join(directory, run)
        # A run with no artifact (cancelled before the record step) is skipped.
        if subprocess.run(["gh", "run", "download", run, "-p", "stress-yield-*", "-D", target], capture_output=True).returncode == 0:
            found.extend(
                os.path.join(base, name) for base, _, names in os.walk(target) for name in names if name == "yield.tsv"
            )
    return found


def main(argv: list[str]) -> int:
    if argv[:1] == ["record"] and len(argv) >= 3:
        flags = dict(zip(argv[3::2], argv[4::2]))
        known_path = flags.pop("--known", os.path.join(HERE, "known-flakes.tsv"))
        date = flags.pop("--date", datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d"))
        try:
            row = record(argv[1], argv[2], flags["--sha"], flags["--run-id"], int(flags["--job-seconds"]), date, known_path)
        except KeyError as missing:
            print(f"stress-yield.py: missing {missing}", file=sys.stderr)
            return 2
        append_row(os.path.join(argv[1], "yield.tsv"), row)
        print(f"- {row['kind']}: {row['status']}, {row['failures']} failing tests ({row['known']} known fixed), "
              f"{row['wall_s']} s in the tool, {row['runner_min']} runner-min")
        return 0
    if argv[:1] == ["report"] and len(argv) >= 2:
        if argv[1] == "--download" and len(argv) == 3:
            with tempfile.TemporaryDirectory() as directory:
                print(report(read_rows(download(int(argv[2]), directory))))
            return 0
        print(report(read_rows(argv[1:])))
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
