# SPDX-License-Identifier: GPL-3.0-or-later
# Procmon helper for the sandbox research trace.
#   trace.py mkcfg <out.pmc>            filter: only msb.exe, drop the rest at capture
#   trace.py summary <in.pml> <out.tsv> what msb.exe touched, aggregated (operation, path class, result)
import collections
import os
import re
import sys

from procmon_parser import ProcmonLogsReader, Rule, dump_configuration


def mkcfg(out):
    cfg = {
        "FilterRules": [Rule("Process_Name", "is", "msb.exe", "include")],
        "DestructiveFilter": 1,
    }
    with open(out, "wb") as f:
        dump_configuration(cfg, f)


def norm(path, temp, prof):
    p = path
    for prefix, name in ((temp, "<RUNNER_TEMP>"), (prof, "<USERPROFILE>")):
        if prefix and p.lower().startswith(prefix.lower()):
            p = name + p[len(prefix):]
    p = re.sub(r"[0-9a-fA-F]{16,}", "<hex>", p)
    p = re.sub(r"\{?[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\}?", "<guid>", p)
    p = re.sub(r"r\d+-\d+[-\w]*", "<run>", p)
    p = re.sub(r"\.tmp\w{6}", ".tmp<x>", p)
    p = re.sub(r"\d{3,}", "<n>", p)
    return p


def summary(pml, out):
    temp = os.environ.get("RUNNER_TEMP", "")
    prof = os.environ.get("USERPROFILE", "")
    counts = collections.Counter()
    access = collections.defaultdict(set)
    cmdlines = set()
    n = 0
    with open(pml, "rb") as f:
        for ev in ProcmonLogsReader(f, should_get_stacktrace=False, should_get_details=True):
            if ev.process.process_name.lower() != "msb.exe":
                continue
            n += 1
            cmd = ev.process.command_line or ""
            cmdlines.add(norm(" ".join(cmd.split()[1:3]), temp, prof))
            role = "machine" if " machine " in f" {cmd} " else "other"
            op = ev.operation if isinstance(ev.operation, str) else getattr(ev.operation, "name", str(ev.operation))
            path = norm(ev.path or "", temp, prof)
            res = ev.result
            key = (role, str(ev.event_class.name), op, path, f"0x{res:08x}" if isinstance(res, int) else str(res))
            counts[key] += 1
            if ev.details and "Desired Access" in ev.details:
                access[(role, path)].add(str(ev.details["Desired Access"]))
    with open(out, "w", encoding="utf-8") as o:
        o.write(f"# {n} msb.exe events; command kinds: {sorted(cmdlines)}\n")
        o.write("role\tclass\toperation\tpath\tresult\tcount\tdesired_access\n")
        for (role, cls, op, path, res), c in sorted(counts.items()):
            acc = ";".join(sorted(access.get((role, path), ())))
            o.write(f"{role}\t{cls}\t{op}\t{path}\t{res}\t{c}\t{acc}\n")


if __name__ == "__main__":
    {"mkcfg": lambda: mkcfg(sys.argv[2]), "summary": lambda: summary(sys.argv[2], sys.argv[3])}[sys.argv[1]]()
