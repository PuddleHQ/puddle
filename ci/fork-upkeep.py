#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Fork upkeep: label every commit the forks carry, and dry-run replay them on upstream's newest code.

Puddle ships patched copies of microsandbox, libkrun, russh, imago and libkrunfw (PuddleHQ forks, pinned
in Cargo.toml). Each commit a fork carries over its upstream is labelled in ci/fork-labels.tsv:

  permanent   a Puddle feature or policy upstream may never take; it is carried for good
  candidate   a fix or feature upstream could take; carried until upstream has it

Usage:
  fork-upkeep.py report [--cargo Cargo.toml] [--labels ci/fork-labels.tsv] [--work DIR] [--out FILE]
  fork-upkeep.py lint   [--labels ci/fork-labels.tsv]
  fork-upkeep.py self-test

`report` clones each fork and its upstream, takes the commits Puddle's pin ships (Cargo.toml) over
their merge base with upstream's branch, and cherry-picks them one by one on upstream's tip in a
scratch checkout (nothing is pushed). Each commit ends as one of:

  applies           applies cleanly
  already upstream  its change is on upstream's tip (the pick comes out empty): drop the commit
  conflict          does not apply: someone has to port it (a commit that builds on a conflicting one conflicts too)
  regenerated       a dependency or version pin the fork's own release job rewrites: not replayed

Exit code: 0 clean, 2 findings (a conflict, an unlabelled commit or a label for a commit that is gone), 1 crash.
"""

import argparse
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

LABELS = ("permanent", "candidate")
REPLAYS = ("pick", "regen")
STATES = ("applies", "already upstream", "conflict", "regenerated")

BOT = ["-c", "user.name=puddle fork upkeep", "-c", "user.email=noreply@users.noreply.github.com",
       "-c", "commit.gpgsign=false"]


@dataclass(frozen=True)
class Fork:
    name: str
    url: str
    upstream_url: str
    upstream_branch: str
    # How Cargo.toml pins the fork: (dependency name, "tag" or "rev"). None: the fork's `branch` tip.
    pin: tuple | None
    branch: str = "puddle"


FORKS = (
    Fork("microsandbox", "https://github.com/PuddleHQ/microsandbox", "https://github.com/superradcompany/microsandbox",
         "main", ("microsandbox", "tag")),
    Fork("libkrun", "https://github.com/PuddleHQ/libkrun", "https://github.com/superradcompany/libkrun",
         "krun", ("msb_krun", "tag")),
    Fork("russh", "https://github.com/PuddleHQ/russh", "https://github.com/Eugeny/russh",
         "main", ("russh", "tag")),
    Fork("imago", "https://github.com/PuddleHQ/imago", "https://github.com/superradcompany/imago",
         "main", ("msb-imago", "rev")),
    # Not a Cargo dependency: the Windows firmware DLL is built from the fork's `puddle` branch.
    Fork("libkrunfw", "https://github.com/PuddleHQ/libkrunfw", "https://github.com/superradcompany/libkrunfw",
         "krunfw", None),
)


@dataclass
class Label:
    fork: str
    label: str
    replay: str
    match: str
    why: str
    line: int
    used: bool = False

    def matches(self, subject):
        if self.match.endswith("*"):
            return subject.startswith(self.match[:-1])
        return subject == self.match


@dataclass
class Row:
    sha: str
    subject: str
    label: Label | None
    state: str = ""
    files: list = field(default_factory=list)


class LabelError(Exception):
    pass


def parse_labels(text, source="labels"):
    """Rows `fork<TAB>label<TAB>replay<TAB>subject<TAB>why`; `#` lines and blanks are comments."""
    out = []
    names = {f.name for f in FORKS}
    for n, line in enumerate(text.splitlines(), 1):
        if not line.strip() or line.lstrip().startswith("#"):
            continue
        cols = line.split("\t")
        if len(cols) != 5:
            raise LabelError(f"{source}:{n}: want 5 tab-separated columns, got {len(cols)}")
        fork, label, replay, match, why = (c.strip() for c in cols)
        if fork not in names:
            raise LabelError(f"{source}:{n}: unknown fork {fork!r} (one of {', '.join(sorted(names))})")
        if label not in LABELS:
            raise LabelError(f"{source}:{n}: label must be one of {', '.join(LABELS)}, not {label!r}")
        if replay not in REPLAYS:
            raise LabelError(f"{source}:{n}: replay must be one of {', '.join(REPLAYS)}, not {replay!r}")
        if not match or not why:
            raise LabelError(f"{source}:{n}: the subject and the reason may not be empty")
        out.append(Label(fork, label, replay, match, why, n))
    return out


def label_for(labels, fork, subject):
    for lab in labels:
        if lab.fork == fork and lab.matches(subject):
            lab.used = True
            return lab
    return None


def parse_pins(cargo_text):
    """The git pins of the forks in the product's Cargo.toml: {dependency: tag-or-rev}."""
    pins = {}
    for m in re.finditer(r'^([A-Za-z0-9_-]+) = \{ git = "https://github\.com/PuddleHQ/[^"]+", (tag|rev) = "([^"]+)"',
                         cargo_text, re.M):
        pins.setdefault(m.group(1), m.group(3))
    return pins


# ---------------------------------------------------------------------------------------------- git
def git(repo, *args, check=True):
    p = subprocess.run(["git", *BOT, *args], cwd=repo, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if check and p.returncode != 0:
        raise RuntimeError(f"git {' '.join(args)} (in {repo}) exited {p.returncode}:\n{p.stdout}\n{p.stderr}")
    return p


def out(repo, *args):
    return git(repo, *args).stdout.strip()


def try_pick(repo, sha):
    """Apply one commit to the checked-out tip. Returns ("applies" | "already upstream", []) or
    ("conflict", conflicted files); the tree is left as it was before on a conflict."""
    p = git(repo, "cherry-pick", "--no-commit", sha, check=False)
    if p.returncode != 0:
        files = out(repo, "diff", "--name-only", "--diff-filter=U").splitlines()
        git(repo, "reset", "-q", "--hard", "HEAD")
        return "conflict", files or ["(no file listed)"]
    if git(repo, "diff", "--cached", "--quiet", check=False).returncode == 0:
        git(repo, "reset", "-q", "--hard", "HEAD")
        return "already upstream", []
    git(repo, "commit", "-q", "-C", sha)
    return "applies", []


def fork_commits(repo, ship, upstream_tip):
    """(base, [(sha, subject)] oldest first): what the shipped ref carries over upstream's branch."""
    base = out(repo, "merge-base", ship, upstream_tip)
    lines = out(repo, "log", "--first-parent", "--no-merges", "--reverse", "--format=%H\t%s", f"{base}..{ship}")
    return base, [tuple(line.split("\t", 1)) for line in lines.splitlines() if line]


def replay(repo, upstream_tip, rows):
    """Cherry-pick the rows on upstream's tip one by one; sets each row's state and conflicted files.
    A conflicting commit is left out and the next one is tried on what has applied so far."""
    git(repo, "checkout", "-q", "--detach", upstream_tip)
    for row in rows:
        if row.label and row.label.replay == "regen":
            row.state = "regenerated"
            continue
        row.state, row.files = try_pick(repo, row.sha)
    return rows


# ------------------------------------------------------------------------------------------ report
@dataclass
class ForkResult:
    fork: Fork
    ship: str
    base: str
    behind: int
    rows: list


def run_fork(fork, ship_ref, labels, work):
    repo = Path(work) / fork.name
    if not repo.exists():
        subprocess.run(["git", "clone", "-q", "--no-tags", fork.url, str(repo)], check=True)
        git(repo, "remote", "add", "upstream", fork.upstream_url)
    git(repo, "fetch", "-q", "--no-tags", "upstream", f"+refs/heads/{fork.upstream_branch}:refs/remotes/upstream/up")
    ship = ship_ref
    if ship_ref and git(repo, "rev-parse", "-q", "--verify", f"{ship_ref}^{{commit}}", check=False).returncode != 0:
        git(repo, "fetch", "-q", "--no-tags", "origin", f"{ship_ref}:refs/shipped/pin", check=False)
        ship = "refs/shipped/pin" if git(repo, "rev-parse", "-q", "--verify", "refs/shipped/pin",
                                          check=False).returncode == 0 else None
    if ship is None:
        raise RuntimeError(f"{fork.name}: cannot find the pinned ref {ship_ref} in {fork.url}")
    ship = out(repo, "rev-parse", f"{ship}^{{commit}}")
    tip = out(repo, "rev-parse", "refs/remotes/upstream/up")
    base, commits = fork_commits(repo, ship, tip)
    rows = [Row(sha, subject, label_for(labels, fork.name, subject)) for sha, subject in commits]
    replay(repo, tip, rows)
    behind = int(out(repo, "rev-list", "--count", f"{base}..{tip}"))
    return ForkResult(fork, ship_ref, base, behind, rows)


def findings(results, labels):
    f = []
    for r in results:
        for row in r.rows:
            if row.label is None:
                f.append(f"{r.fork.name}: `{row.subject}` ({row.sha[:10]}) has no label in ci/fork-labels.tsv")
            elif row.state == "conflict":
                f.append(f"{r.fork.name}: `{row.subject}` ({row.label.label}) conflicts on upstream: "
                         + ", ".join(f"`{x}`" for x in row.files))
        for row in r.rows:
            if row.state == "already upstream":
                f.append(f"{r.fork.name}: `{row.subject}` ({row.label.label if row.label else 'unlabelled'}) "
                         "is already upstream: drop it from the fork")
    seen = {r.fork.name for r in results}
    for lab in labels:
        if lab.fork in seen and not lab.used:
            f.append(f"ci/fork-labels.tsv:{lab.line}: label `{lab.match}` matches no {lab.fork} commit any more")
    return f


def render(results, labels, problems):
    s = ["# Fork upkeep: the forks replayed on upstream", ""]
    perm = sum(1 for r in results for w in r.rows if w.label and w.label.label == "permanent")
    cand = sum(1 for r in results for w in r.rows if w.label and w.label.label == "candidate")
    total = sum(len(r.rows) for r in results)
    s.append(f"{total} fork commits: {perm} permanent, {cand} upstream candidates, {total - perm - cand} unlabelled.")
    s.append("")
    for r in results:
        counts = {st: sum(1 for w in r.rows if w.state == st) for st in STATES}
        s.append(f"## {r.fork.name}")
        s.append("")
        s.append(f"Shipped `{r.ship}` carries {len(r.rows)} commits over `{r.base[:10]}`; upstream `{r.fork.upstream_branch}` "
                 f"has moved {r.behind} commits since. Dry-run replay on its tip: "
                 + ", ".join(f"{n} {st}" for st, n in counts.items() if n) + ".")
        s.append("")
        bad = [w for w in r.rows if w.state in ("conflict", "already upstream")]
        if bad:
            s += ["| commit | label | result | files |", "|---|---|---|---|"]
            for w in bad:
                s.append(f"| `{w.subject}` | {w.label.label if w.label else 'unlabelled'} | {w.state} | "
                         + (", ".join(f"`{x}`" for x in w.files) or "-") + " |")
            s.append("")
    s.append("## Needs a person" if problems else "## Nothing to do")
    s.append("")
    s += [f"- {p}" for p in problems] or ["Every commit is labelled and the stacks replay without conflicts."]
    s.append("")
    s.append("Made by `ci/fork-upkeep.py` (weekly workflow `fork-upkeep.yml`). The replay is a dry run: nothing is pushed. "
             "The microsandbox fork's own release job rebuilds the real stack on each upstream release.")
    return "\n".join(s) + "\n"


def cmd_report(args):
    labels = parse_labels(Path(args.labels).read_text(), args.labels)
    pins = parse_pins(Path(args.cargo).read_text())
    work = args.work or tempfile.mkdtemp(prefix="fork-upkeep-")
    results = []
    for fork in FORKS:
        ref = pins.get(fork.pin[0]) if fork.pin else f"origin/{fork.branch}"
        if ref is None:
            raise RuntimeError(f"{args.cargo} has no git pin for {fork.pin[0]}")
        results.append(run_fork(fork, ref, labels, work))
    problems = findings(results, labels)
    text = render(results, labels, problems)
    if args.out:
        Path(args.out).write_text(text)
    print(text)
    return 2 if problems else 0


def cmd_lint(args):
    labels = parse_labels(Path(args.labels).read_text(), args.labels)
    seen = {}
    for lab in labels:
        key = (lab.fork, lab.match)
        if key in seen:
            raise LabelError(f"{args.labels}:{lab.line}: duplicate of line {seen[key]}")
        seen[key] = lab.line
    print(f"{len(labels)} labels ok")
    return 0


# ----------------------------------------------------------------------------------------- self-test
def _commit(repo, name, text, msg):
    (repo / name).write_text(text)
    git(repo, "add", name)
    git(repo, "commit", "-q", "-m", msg)
    return out(repo, "rev-parse", "HEAD")


def self_test():
    ok = True

    def check(cond, what):
        nonlocal ok
        print(("ok   " if cond else "FAIL ") + what)
        ok &= bool(cond)

    # parsing
    lab = parse_labels("# c\n\nmicrosandbox\tpermanent\tpick\tci(fork):*\tfork CI\n")
    check(len(lab) == 1 and lab[0].matches("ci(fork): x") and not lab[0].matches("ci: x"), "prefix match")
    for bad, why in (("x\tpermanent\tpick\ta\tb", "unknown fork"), ("russh\tmaybe\tpick\ta\tb", "bad label"),
                     ("russh\tpermanent\tskip\ta\tb", "bad replay"), ("russh\tpermanent\tpick\ta", "short row"),
                     ("russh\tpermanent\tpick\ta\t", "empty reason")):
        try:
            parse_labels(bad)
            check(False, f"rejects {why}")
        except LabelError:
            check(True, f"rejects {why}")
    pins = parse_pins('microsandbox = { git = "https://github.com/PuddleHQ/microsandbox", tag = "v1-puddle.2", x = 1 }\n'
                      'msb-imago = { git = "https://github.com/PuddleHQ/imago", rev = "abc" }\n'
                      'other = { git = "https://github.com/x/y", tag = "no" }\n')
    check(pins == {"microsandbox": "v1-puddle.2", "msb-imago": "abc"}, "reads tag and rev pins, ignores other owners")

    # replay on a fixture: upstream moved on after the fork's base
    with tempfile.TemporaryDirectory() as tmp:
        repo = Path(tmp) / "r"
        repo.mkdir()
        git(repo, "init", "-q", "-b", "main")
        _commit(repo, "a.txt", "one\ntwo\nthree\n", "base a")
        _commit(repo, "b.txt", "b\n", "base b")
        base = out(repo, "rev-parse", "HEAD")
        git(repo, "checkout", "-q", "-b", "fork", base)
        _commit(repo, "c.txt", "new file\n", "fix: clean")
        _commit(repo, "a.txt", "one\nTWO\nthree\n", "fix: conflicts")
        _commit(repo, "b.txt", "b\nmore\n", "fix: cascades")
        (repo / "d.txt").write_text("d\n")
        git(repo, "add", "d.txt")
        git(repo, "commit", "-q", "-m", "fix: upstream took it")
        _commit(repo, "e.txt", "pins\n", "build: pins")
        ship = out(repo, "rev-parse", "HEAD")
        git(repo, "checkout", "-q", "-b", "up", base)
        _commit(repo, "a.txt", "one\n2\nthree\n", "upstream edits a")
        _commit(repo, "b.txt", "b\nother\n", "upstream edits b")
        _commit(repo, "d.txt", "d\n", "upstream adds d")
        tip = out(repo, "rev-parse", "HEAD")

        b, commits = fork_commits(repo, ship, tip)
        check(b == base and [s for _, s in commits] == ["fix: clean", "fix: conflicts", "fix: cascades",
                                                        "fix: upstream took it", "build: pins"],
              "fork commits are the shipped ref over the merge base, oldest first")
        labs = parse_labels("\n".join(f"microsandbox\t{k}\t{r}\t{m}\twhy" for k, r, m in (
            ("candidate", "pick", "fix: clean"), ("permanent", "pick", "fix: conflicts"),
            ("candidate", "pick", "fix: cascades"), ("candidate", "pick", "fix: upstream took it"),
            ("permanent", "regen", "build:*"), ("permanent", "pick", "fix: gone"))))
        rows = [Row(sha, s, label_for(labs, "microsandbox", s)) for sha, s in commits]
        replay(repo, tip, rows)
        got = {r.subject: r.state for r in rows}
        check(got == {"fix: clean": "applies", "fix: conflicts": "conflict", "fix: cascades": "conflict",
                      "fix: upstream took it": "already upstream", "build: pins": "regenerated"},
              f"replay states {got}")
        check(rows[1].files == ["a.txt"] and rows[2].files == ["b.txt"], "a conflict names its files")
        check(out(repo, "status", "--porcelain") == "", "the checkout is clean after conflicts")
        check(out(repo, "ls-files").split() == ["a.txt", "b.txt", "c.txt", "d.txt"],
              "the clean commit is kept, the conflicting ones and the regenerated one are not")

        # a second fork whose commits depend on each other: the dependent one conflicts too
        git(repo, "checkout", "-q", "-b", "fork2", base)
        _commit(repo, "a.txt", "one\nTWO\nthree\n", "step1")
        _commit(repo, "a.txt", "one\nTWO!\nthree\n", "step2")
        s2 = out(repo, "rev-parse", "HEAD")
        _, commits2 = fork_commits(repo, s2, tip)
        rows2 = [Row(sha, s, None) for sha, s in commits2]
        replay(repo, tip, rows2)
        check([r.state for r in rows2] == ["conflict", "conflict"], "a commit that builds on a conflicting one conflicts")

        probs = findings([ForkResult(FORKS[0], "x", base, 3, rows)], labs)
        check(any("`fix: conflicts` (permanent) conflicts" in p and "a.txt" in p for p in probs), "finding: a conflict")
        check(any("already upstream" in p for p in probs), "finding: already upstream")
        check(any("fix: gone" in p and "no microsandbox commit" in p for p in probs), "finding: stale label")
        probs2 = findings([ForkResult(FORKS[0], "x", base, 3, rows2)], [])
        check(sum("no label" in p for p in probs2) == 2, "finding: unlabelled commits")
        text = render([ForkResult(FORKS[0], "v1", base, 3, rows)], labs, probs)
        check("## Needs a person" in text and "| `fix: conflicts` | permanent | conflict | `a.txt` |" in text,
              "report table")
        clean = render([ForkResult(FORKS[0], "v1", base, 3, rows[:1])], labs[:1], [])
        check("## Nothing to do" in clean and "1 fork commits: 0 permanent, 1 upstream candidates, 0 unlabelled" in clean,
              "clean report")

    # the shipped label file is well formed
    here = Path(__file__).resolve().parent / "fork-labels.tsv"
    if here.exists():
        parse_labels(here.read_text(), str(here))
        check(True, "ci/fork-labels.tsv parses")
    print("self-test " + ("passed" if ok else "FAILED"))
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    here = Path(__file__).resolve().parent
    rp = sub.add_parser("report")
    rp.add_argument("--cargo", default=str(here.parent / "Cargo.toml"))
    rp.add_argument("--labels", default=str(here / "fork-labels.tsv"))
    rp.add_argument("--work", help="directory for the clones (default: a new temp directory)")
    rp.add_argument("--out", help="also write the Markdown report here")
    lp = sub.add_parser("lint")
    lp.add_argument("--labels", default=str(here / "fork-labels.tsv"))
    sub.add_parser("self-test")
    args = ap.parse_args()
    try:
        if args.cmd == "self-test":
            return self_test()
        return {"report": cmd_report, "lint": cmd_lint}[args.cmd](args)
    except LabelError as err:
        print(f"error: {err}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
