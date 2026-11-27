#!/usr/bin/env bash
# SPDX-License-Identifier: AGPL-3.0-or-later
# The full-steps guard and the checks that the workflows are wired as scope.sh expects (ci/scope-test.sh).
# Sourced by ci/scope-test.sh at script scope: it shares the main script's variables, functions and `fail`.
# shellcheck shell=bash disable=SC2034,SC2154 # `fail`, `bad` and the variables are shared with ci/scope-test.sh, which sources this file
# full-steps: off the light mode every guarded step must have succeeded; in the light mode anything goes.
steps_case() { # steps_case <want> <label> <args>...
    local exp=$1 label=$2 rc=0
    shift 2
    env -u PUDDLE_QUEUE -u PR_BASE_REF "${STEPS_ENV[@]}" "$scope" full-steps "$@" >/dev/null 2>&1 || rc=$?
    [ "$rc" = "$exp" ] || bad "full-steps: $label: exit $rc, want $exp"
}
for ev in push schedule workflow_dispatch merge_group pull_request; do
    STEPS_ENV=("GITHUB_EVENT_NAME=$ev" PR_BASE_REF=develop)
    steps_case 0 "$ev: all ran" git-env=success notices=success
    steps_case 1 "$ev: one skipped" git-env=skipped notices=success
    steps_case 1 "$ev: one failed" git-env=success notices=failure
    steps_case 1 "$ev: one lost" git-env= notices=success
    STEPS_ENV=("GITHUB_EVENT_NAME=$ev" PR_BASE_REF=develop PUDDLE_QUEUE=on)
    if [ "$ev" = pull_request ]; then
        steps_case 0 "light: skipped" git-env=skipped notices=skipped
    else
        steps_case 1 "$ev with PUDDLE_QUEUE=on: one skipped" git-env=skipped notices=success
    fi
done
STEPS_ENV=(GITHUB_EVENT_NAME=pull_request PR_BASE_REF=main PUDDLE_QUEUE=on)
steps_case 1 "pull request to main with PUDDLE_QUEUE=on: one skipped" git-env=skipped
[ "$(NEEDS='' GITHUB_EVENT_NAME=pull_request "$scope" verdict x=y >/dev/null 2>&1; echo $?)" = 1 ] || bad "verdict: empty NEEDS must fail"

# 7. The workflows are wired to match their result job's verdict (needs PyYAML).
if python3 -c 'import yaml' 2>/dev/null; then
    python3 -I - "$here/../.github/workflows" <<'PY' || fail=1
import re, sys, yaml
d = sys.argv[1]
results = {  # workflow: (result job id, required name, verdict specs as the matrix above tests them)
    "ci": ("linux", "linux (all gates)", "rust=ci_rust,pushskip gates=ci_rust,pushskip hooks=ci_rust,pushskip msvc=msvc,pushskip light=!ci_rust ui=ui,pushskip ui-webkit=ui,queue,pushskip,defer:defer_linux_webkit unit=unit unit-ui=unit_ui"),
    "windows": ("windows", "windows-msvc (build, clippy, tests)", "clippy=win_rust,queue,defer:defer_win_clippy test-build=win_rust tests=win_rust docs=win_rust ui-e2e=ui,queue,defer:defer_win_ui ui-unit=ui,queue,defer:defer_win_ui"),
    "vm-linux": ("vm-linux", "vm-linux (KVM, msb microVMs)", "--nightly-skips build=vm,queue agent=vm,queue tests=vm,queue"),
    "vm-windows": ("vm-windows", "vm-windows (WHP, msb microVMs)", "--nightly-skips build=vm,queue,defer:defer_vm_windows agent=vm,queue,defer:defer_vm_windows tests=vm,queue,defer:defer_vm_windows"),
}
LIGHT = "needs.scope.outputs.light != 'true'"
PUSHSKIP = "needs.queue-tested.outputs.skip != 'true'"
bad = []
def flat(x):
    return str(x).replace(" ", "")
for wf, (rid, rname, want_specs) in results.items():
    doc = yaml.safe_load(open(f"{d}/{wf}.yml"))
    on = doc.get("on", doc.get(True)) or {}
    # Every workflow that reports a required check runs on both events, or the check stays pending.
    if "pull_request" not in on:
        bad.append(f"{wf}: no pull_request trigger")
    # The nightly tier runs on no push trigger; a release PR to main must still reach the VM and Windows jobs.
    if wf != "ci" and "main" not in (on.get("pull_request") or {}).get("branches", []):
        bad.append(f"{wf}: pull_request must include branch main (the release PR runs the nightly-tier jobs)")
    if wf != "ci" and "push" in on:
        bad.append(f"{wf}: no push trigger (the nightly tier runs nightly and on a PR to main only)")
    mg = on.get("merge_group") if isinstance(on, dict) else None
    if not mg or mg.get("types") != ["checks_requested"]:
        bad.append(f"{wf}: needs `merge_group: types: [checks_requested]`")
    jobs = doc["jobs"]
    res = jobs[rid]
    if res.get("name") != rname:
        bad.append(f"{wf}: result job name {res.get('name')!r}, want {rname!r}")
    # ci.yml's nightly (schedule) runs only its advisories jobs, so its result job skips on that event.
    want_if = "${{always()&&github.event_name!='schedule'}}" if wf == "ci" else "${{always()}}"
    if flat(res.get("if", "")) != want_if:
        bad.append(f"{wf}: result job must run with always()")
    needs = res["needs"]
    sc = jobs.get("scope")
    if not sc or "ci/scope.sh" not in str(sc["steps"]):
        bad.append(f"{wf}: no scope job running ci/scope.sh")
        continue
    if "light" not in sc.get("outputs", {}):
        bad.append(f"{wf}: scope job has no output light")
    scope_env = [s for s in sc["steps"] if s.get("run") == "ci/scope.sh"][0].get("env", {})
    for k, v in (("PUDDLE_QUEUE", "${{ vars.PUDDLE_QUEUE }}"),
                 ("PUDDLE_LIGHT_CI", "${{ vars.PUDDLE_LIGHT_CI }}"),
                 ("MERGE_GROUP_BASE_SHA", "${{ github.event.merge_group.base_sha }}"),
                 ("MERGE_GROUP_HEAD_SHA", "${{ github.event.merge_group.head_sha }}"),
                 ("PR_BASE_REF", "${{ github.event.pull_request.base.ref }}")):
        if scope_env.get(k) != v:
            bad.append(f"{wf}: scope step env {k}={scope_env.get(k)!r}, want {v!r}")
    vstep = [s for s in res["steps"] if "run" in s][-1]
    venv = vstep.get("env", {})
    tiered = "defer:" in want_specs
    for k, v in (("PUDDLE_QUEUE", "${{ vars.PUDDLE_QUEUE }}"), ("PR_BASE_REF", "${{ github.event.pull_request.base.ref }}")):
        if venv.get(k) != v:
            bad.append(f"{wf}: verdict step env {k}={venv.get(k)!r}, want {v!r}")
    if tiered and (venv.get("PUDDLE_TIER") != "${{ vars.PUDDLE_TIER }}" or scope_env.get("PUDDLE_TIER") != "${{ vars.PUDDLE_TIER }}"):
        bad.append(f"{wf}: the scope and verdict steps need PUDDLE_TIER from vars")
    if "pushskip" in want_specs and venv.get("PUDDLE_QUEUE_SKIP_PUSH") != "${{ vars.PUDDLE_QUEUE_SKIP_PUSH }}":
        bad.append(f"{wf}: verdict step needs PUDDLE_QUEUE_SKIP_PUSH from vars")
    m = re.search(r"ci/scope\.sh verdict (.*)", vstep["run"])
    got_specs = " ".join(t.replace("'", "") for t in m.group(1).split()) if m else ""
    if got_specs != want_specs:
        bad.append(f"{wf}: verdict specs {got_specs!r}, want {want_specs!r} (change the matrix with them)")
    specs = {}
    for tok in (t for t in got_specs.split() if not t.startswith("--")):
        j, k = tok.split("=", 1)
        specs[j] = k
    for j, k in specs.items():
        if j not in needs:
            bad.append(f"{wf}: verdict names {j} which the result job does not need")
        key, *flags = k.split(",")
        key = key.lstrip("!")
        jn = jobs[j].get("needs") or []
        jn = [jn] if isinstance(jn, str) else jn
        # A job behind a scoped job of the same spec (a test partition after its build) is skipped
        # by that job's skip; the others carry the condition themselves.
        behind = any(specs.get(x) == k for x in jn)
        cond = str(jobs[j].get("if", ""))
        want = f"needs.scope.outputs.{key} " + ("== 'false'" if k.startswith("!") else "!= 'false'")
        if not behind and (want not in cond or "!cancelled()" not in cond):
            bad.append(f"{wf}: job {j} if={cond!r}, want !cancelled() and {want!r}")
        if not behind and "scope" not in jn:
            bad.append(f"{wf}: job {j} does not need scope")
        # A queue-only job skips exactly in the light mode; a pushskip job exactly when queue-tested says so.
        if not behind and ("queue" in flags) != (LIGHT in cond):
            bad.append(f"{wf}: job {j}: the `queue` mark and {LIGHT!r} in its if must go together")
        if not behind and ("pushskip" in flags) != (PUSHSKIP in cond and "queue-tested" in jn):
            bad.append(f"{wf}: job {j}: the `pushskip` mark and {PUSHSKIP!r} (needing queue-tested) must go together")
        if key not in sc.get("outputs", {}):
            bad.append(f"{wf}: scope job has no output {key}")
        # A job moved to the nightly tier skips exactly when scope says `defer_...` is true (and only then).
        dkeys = [f[6:] for f in flags if f.startswith("defer:")]
        if not behind and bool(dkeys) != ("outputs.defer_" in cond):
            bad.append(f"{wf}: job {j}: the `defer:` mark and a defer_ output in its if must go together")
        for dk in dkeys:
            if not behind and f"needs.scope.outputs.{dk} != 'true'" not in cond:
                bad.append(f"{wf}: job {j} if={cond!r} lacks needs.scope.outputs.{dk} != 'true'")
            if dk not in sc.get("outputs", {}):
                bad.append(f"{wf}: scope job has no output {dk}")
    for j in needs:
        if j == "scope" or j in specs:
            continue
        # Unscoped jobs: the release build, the queue-tested lookup and the advisories checks, each with
        # its own event rule (advisories: not on a pull request to develop or a merge queue run, so a new
        # RustSec or npm entry fails no such run).
        cond = flat(jobs[j].get("if", ""))
        if wf == "ci" and j in ("advisories", "advisories-npm"):
            if ("github.event.pull_request.base.ref=='main'" not in cond or "github.event_name!='pull_request'" not in cond
                    or "github.event_name!='merge_group'&&" not in cond):
                bad.append(f"ci: job {j} if={cond!r} must skip exactly on a merge queue run and a pull request not to main")
        elif wf == "ci" and j == "queue-tested":
            for part in ("github.event_name=='push'", "github.ref=='refs/heads/develop'", "vars.PUDDLE_QUEUE_SKIP_PUSH=='on'"):
                if part not in cond:
                    bad.append(f"ci: job queue-tested if={cond!r} lacks {part}")
            # In the light CI mode a green queue run tested only the unit tier: the lookup must know the mode.
            if "PUDDLE_LIGHT_CI: ${{ vars.PUDDLE_LIGHT_CI }}" not in yaml.safe_dump(jobs[j]["steps"], width=1000).replace("'", ""):
                bad.append("ci: the queue-tested lookup step needs PUDDLE_LIGHT_CI from vars")
        elif not (wf == "windows" and j == "release"):
            bad.append(f"{wf}: needed job {j} is neither scoped nor the release build")
    if wf == "windows" and "merge_group" in flat(jobs["release"].get("if", "")):
        bad.append("windows: the release build has no merge queue twin and must not run there")
    # Every other job of the workflow is reached through the result job (directly or via needs).
    for j, spec in jobs.items():
        if j in (rid, "scope") or j in needs:
            continue
        n = spec.get("needs")
        n = [n] if isinstance(n, str) else (n or [])
        if not any(x in needs for x in n):
            bad.append(f"{wf}: job {j} is not covered by the result job")
    # Steps the light mode skips inside a running job: the verdict cannot see them, so the job's
    # full-steps guard names each one (by its id) and reads the event's own variables. A job's own
    # environment never narrows in the light mode: a skipped job (`ui-webkit`) is what the verdict sees.
    for j, spec in jobs.items():
        steps = spec.get("steps") or []
        light_steps = [s for s in steps if "outputs.light" in str(s.get("if", ""))]
        if "outputs.light" in str(spec.get("env", "")):
            bad.append(f"{wf}: job {j} narrows its environment in the light mode, which no guard can see (skip the job instead)")
        if not light_steps:
            continue
        guards = [s for s in steps if "ci/scope.sh full-steps" in str(s.get("run", ""))]
        if len(guards) != 1:
            bad.append(f"{wf}: job {j} skips steps in the light mode but has {len(guards)} full-steps guards")
            continue
        g = guards[0]
        tail = [x for x in steps if x.get("uses") != "./.github/actions/cancel-group"]
        if tail[-1] is not g:
            bad.append(f"{wf}: job {j}: the full-steps guard must be the last step (before the cancel-group step)")
        if flat(g.get("if", "")) != "${{!cancelled()}}":
            bad.append(f"{wf}: job {j}: the full-steps guard must run with !cancelled()")
        genv = g.get("env", {})
        if genv.get("PUDDLE_QUEUE") != "${{ vars.PUDDLE_QUEUE }}" or genv.get("PR_BASE_REF") != "${{ github.event.pull_request.base.ref }}":
            bad.append(f"{wf}: job {j}: the full-steps guard needs PUDDLE_QUEUE and PR_BASE_REF")
        for s in light_steps:
            sid = s.get("id")
            if not sid or f"{sid}=${{{{ steps.{sid}.outcome }}}}" not in g["run"]:
                bad.append(f"{wf}: job {j}: step {s.get('name')!r} skips in the light mode but the guard does not check it")
    # The Linux browsers: the `ui` job runs the end-to-end tests on Chromium, `ui-webkit` on WebKit, and nothing else
    # narrows them, so a pull request to main runs both (webkit.yml runs WebKit nightly for the skips above).
    if wf == "ci":
        for j, want in (("ui", "chromium"), ("ui-webkit", "webkit")):
            got = (jobs[j].get("env") or {}).get("PUDDLE_E2E_PROJECTS")
            if got != want:
                bad.append(f"ci: job {j} runs PUDDLE_E2E_PROJECTS={got!r}, want {want!r}")
            if not any("scripts/check.sh ui-e2e" in str(s.get("run", "")) for s in jobs[j]["steps"]):
                bad.append(f"ci: job {j} does not run the ui-e2e gate")
        # The two browser entries have their own keys (a shared one would give the WebKit job a Chromium-only hit),
        # webkit.yml restores the WebKit one, and the queue-tested lookup covers both.
        text = open(f"{d}/ci.yml").read()
        if text.count("key: playwright-webkit-${{ runner.os }}-${{ hashFiles('ui/package-lock.json') }}") < 3:
            bad.append("ci: the ui-webkit job and queue-tested must use the key playwright-webkit-... for the WebKit browsers")
        if "key: playwright-webkit-${{ runner.os }}" not in open(f"{d}/webkit.yml").read():
            bad.append("webkit: must restore the key playwright-webkit-... the ui-webkit job saves")
# The nightly gates of the VM jobs, Windows UI and clippy count only nightly and manual runs: a merge
# queue run on develop's commit must not skip the soak tests or the nightly UI run.
for wf in ("windows", "vm-linux", "vm-windows"):
    gates = re.findall(r"run: ci/should-run\.sh \S+(.*)", open(f"{d}/{wf}.yml").read())
    if not gates:
        bad.append(f"{wf}: no should-run.sh gate found")
    for g in gates:
        if g.strip() != "--events schedule,workflow_dispatch":
            bad.append(f"{wf}: nightly gate {g.strip()!r} must be exactly '--events schedule,workflow_dispatch'")

# The Windows tests: the partition jobs run the archive the build job shares, the matrix and the `hash:` split
# have one size, the build archives the workspace the way the partitions run it, and every job that opens
# a browser or the shell stops the storage service.
wj = yaml.safe_load(open(f"{d}/windows.yml"))["jobs"]
wtext2 = open(f"{d}/windows.yml").read()
if wj["tests"].get("needs") != "test-build":
    bad.append("windows: the partition job must need exactly test-build")
parts = wj["tests"]["strategy"]["matrix"]["partition"]
if len(parts) != 4 or parts != list(range(1, len(parts) + 1)):
    bad.append(f"windows: partitions {parts}, want 1..4 (the job names say 'of 4')")
if "of 4)" not in wj["tests"]["name"] or "of 4)" not in str([s.get("name") for s in wj["tests"]["steps"]]):
    bad.append("windows: the partition job and step names must say 'of 4' (the matrix size)")
if 'hash:${PARTITION}/${PARTITIONS}' not in wtext2 or "PARTITIONS: ${{ strategy.job-total }}" not in wtext2:
    bad.append("windows: the partitions must split by nextest `--partition hash:` over strategy.job-total")
up = [s for s in wj["test-build"]["steps"] if s.get("uses", "").startswith("actions/upload-artifact")]
dn = [s for s in wj["tests"]["steps"] if s.get("uses", "").startswith("actions/download-artifact")]
if len(up) != 1 or len(dn) != 2 or {s["with"]["name"] for s in dn} != {up[0]["with"]["name"]}:
    bad.append("windows: the build job's upload and the partition job's download (first try and second try) must name the same artifact")
if [s.get("timeout-minutes") for s in dn] != [8, 8] or dn[0].get("continue-on-error") is not True or not dn[1]["name"].endswith("(second try)"):
    bad.append("windows: the archive download needs a step limit, continue-on-error and a second try (a hung download must not hold the job to its limit)")
if "cargo nextest archive --workspace --all-features --locked --profile ci" not in wtext2:
    bad.append("windows: the build job must archive the workspace with --all-features --locked --profile ci")
if 'export RUSTUP_TOOLCHAIN' not in wtext2 or "rust-toolchain.toml" not in str(wj["tests"]["steps"]):
    bad.append("windows: the partitions must export RUSTUP_TOOLCHAIN from rust-toolchain.toml (dev_override runs cargo)")
if "nextest run --profile ci" not in wtext2:
    bad.append("windows: the partitions must run the ci profile")
for j in ("ui-e2e", "ui-unit"):
    if not any("Stop-Service StorSvc" in str(s.get("run", "")) for s in wj[j]["steps"]):
        bad.append(f"windows: job {j} must stop the storage service")
if not any((s.get("env") or {}).get("TEMP") == "${{ runner.temp }}" and "ui-e2e" in str(s.get("run", "")) for s in wj["ui-e2e"]["steps"]):
    bad.append("windows: the Edge end-to-end step must keep TEMP on the runner's temp disk")
for j in ("docs", "ui-e2e", "ui-unit"):
    rc = [s for s in wj[j]["steps"] if str(s.get("uses", "")).startswith("Swatinem/rust-cache")]
    if len(rc) != 1 or rc[0]["with"].get("shared-key") != "windows" or rc[0]["with"].get("save-if") is not False:
        bad.append(f"windows: job {j} must restore the `windows` Rust cache without saving it")
sv = [s for s in wj["test-build"]["steps"] if str(s.get("uses", "")).startswith("Swatinem/rust-cache")]
if len(sv) != 1 or sv[0]["with"].get("shared-key") != "windows":
    bad.append("windows: the build job saves the `windows` Rust cache")

# The WebKit nightly (webkit.yml) is what the queue's Chromium-only run leans on: it must exist, run on a
# schedule, never be narrowed, never save the shared browser cache (a WebKit-only entry would starve the
# Chromium job that restores it) and be watched by the red-signal workflow.
wk = yaml.safe_load(open(f"{d}/webkit.yml"))
won = wk.get("on", wk.get(True)) or {}
if sorted(won) != ["schedule", "workflow_dispatch"]:
    bad.append(f"webkit: triggers {sorted(won)}, want exactly schedule and workflow_dispatch (a pull request to main runs both browsers in ci.yml)")
wtext = open(f"{d}/webkit.yml").read()
if "run: ci/should-run.sh webkit.yml\n" not in wtext:
    bad.append("webkit: the nightly gate must be `ci/should-run.sh webkit.yml`")
if "actions/cache/save" in wtext:
    bad.append("webkit: must not save the Playwright cache (ci.yml's `ui-webkit` job saves that key)")
for j, spec in wk["jobs"].items():
    if (spec.get("env") or {}).get("PUDDLE_E2E_PROJECTS") != "webkit":
        bad.append(f"webkit: job {j} must run PUDDLE_E2E_PROJECTS=webkit")
    if not any("scripts/check.sh ui-e2e" in str(x.get("run", "")) for x in spec.get("steps", [])):
        bad.append(f"webkit: job {j} does not run scripts/check.sh ui-e2e")
ns = yaml.safe_load(open(f"{d}/nightly-signal.yml"))
if "webkit" not in ((ns.get("on", ns.get(True)) or {}).get("workflow_run") or {}).get("workflows", []):
    bad.append("nightly-signal: webkit is not watched")
# Fail fast in the merge queue (ci/cancel-group.sh): every job that does work ends with the cancel-group step,
# guarded by failure() on a merge_group event and best effort, and has `actions: write`; the result jobs, the
# queue-tested lookup and the advisories checks (which skip on a merge queue run) do not.
for wf, (rid, rname, want_specs) in results.items():
    wjobs = yaml.safe_load(open(f"{d}/{wf}.yml"))["jobs"]
    for j, spec in wjobs.items():
        steps = spec.get("steps") or []
        last = steps[-1] if steps else {}
        has = last.get("uses") == "./.github/actions/cancel-group"
        exempt = j == rid or (wf == "ci" and j in ("queue-tested", "advisories", "advisories-npm"))
        if exempt:
            if any(x.get("uses") == "./.github/actions/cancel-group" for x in steps):
                bad.append(f"{wf}: job {j} must not cancel the group")
            continue
        if not has:
            bad.append(f"{wf}: job {j} does not end with the cancel-group step")
            continue
        if flat(last.get("if", "")) != "${{failure()&&github.event_name=='merge_group'}}" or last.get("continue-on-error") is not True:
            bad.append(f"{wf}: job {j}: the cancel-group step needs `failure() && merge_group` and continue-on-error")
        if (spec.get("permissions") or {}).get("actions") != "write":
            bad.append(f"{wf}: job {j}: the cancel-group step needs `actions: write` on the job")
# The VM workflows fail on a missing hypervisor on a pull request and a merge queue run.
if "ci/vm-skip-policy.sh" not in open(f"{d}/vm-linux.yml").read():
    bad.append("vm-linux: the KVM preflight does not use ci/vm-skip-policy.sh")
# vm-windows uses the shared WHP preflight; its default `fail-events` must name both events, and the partition job must not override it.
whp = yaml.safe_load(open(f"{d}/../actions/whp-preflight/action.yml"))
if set(str(whp["inputs"]["fail-events"]["default"]).split()) != {"pull_request", "merge_group"}:
    bad.append("vm-windows: the WHP preflight does not fail on a pull request and a merge queue run by default")
part = [x for x in yaml.safe_load(open(f"{d}/vm-windows.yml"))["jobs"]["tests"]["steps"] if x.get("id") == "whp"]
if len(part) != 1 or part[0].get("uses") != "./.github/actions/whp-preflight" or "fail-events" in (part[0].get("with") or {}):
    bad.append("vm-windows: the partition job's WHP preflight must be the shared action with its default fail-events")
for b in bad:
    print("FAIL:", b, file=sys.stderr)
sys.exit(1 if bad else 0)
PY
else
    echo "note: PyYAML missing, workflow wiring not checked" >&2
fi

# 7b. What `vm_unrelated` (ci/scope.sh) assumes about the real tree, so a later change cannot make it wrong silently:
# the VM workflows call no other workflow, and no top-level non-VM test file of a crate holds a `vm_`
# test or is a module of a `vm_*` test file (`mod <name>;` or `#[path]`).
root="$here/.."
if grep -Ehn "uses: *\./\.github/workflows/|workflow_call|workflow_run" "$root/.github/workflows/vm-linux.yml" "$root/.github/workflows/vm-windows.yml" >&2; then
    bad "a VM workflow calls another workflow: vm_unrelated in ci/scope.sh would skip them for it"
fi
for f in "$root"/crates/*/tests/*.rs; do
    [ -e "$f" ] || continue
    case ${f##*/} in vm_*.rs | support.rs | common.rs | mod.rs) continue ;; esac
    [ "${f#"$root"/crates/puddle-vm-tests/}" != "$f" ] && continue
    stem=${f##*/}
    stem=${stem%.rs}
    if grep -Eq 'fn vm_[a-z0-9_]*' "$f"; then bad "$f holds a vm_ test, but vm_unrelated treats it as a non-VM file"; fi
    for v in "${f%/*}"/vm_*.rs; do
        [ -e "$v" ] || continue
        if grep -Eq "^[[:space:]]*(pub +)?mod +$stem *;|#\[path *= *\"$stem\.rs\"" "$v"; then bad "$v includes $f, but vm_unrelated treats it as a non-VM file"; fi
    done
done

# 8. The crate sets against the real graph (opt in: needs cargo; CI's Rust job turns it on).
if [ "${SCOPE_TEST_METADATA:-0}" = 1 ]; then
    meta="$tmp/meta.json"
    if (cd "$here/.." && cargo metadata --no-deps --offline --format-version 1 >"$meta" 2>"$tmp/meta.err"); then
        python3 -I - "$meta" "$scope" <<'PY' || fail=1
import json, re, sys
meta = json.load(open(sys.argv[1]))
src = open(sys.argv[2]).read()
pk = {p["name"]: p for p in meta["packages"]}
def closure(roots):
    seen, st = set(roots), list(roots)
    while st:
        for d in pk[st.pop()]["dependencies"]:
            if d["name"] in pk and d["name"] not in seen:
                seen.add(d["name"]); st.append(d["name"])
    return seen
def listed(var):
    return set(re.search(var + r'=" ([^"]*) "', src).group(1).split())
vm_roots = ["puddle-vm-tests", "puddle-compute-msb", "puddle-boot", "puddle-workspace", "puddle-lifecycle", "puddle-host", "puddle-agent"]
env = re.search(r"VM_CRATES: (.*)", open(sys.argv[2].replace("scope.sh", "../.github/workflows/vm-linux.yml")).read()).group(1)
want_vm = set(re.findall(r"-p (\S+)", env)) | {"puddle-agent"}
bad = []
if want_vm != set(vm_roots):
    bad.append(f"the VM crates in vm-linux.yml are {sorted(want_vm)}; scope-test.sh roots are {sorted(vm_roots)}")
for var, roots in (("NO_VM", vm_roots), ("NO_UI", ["puddle-e2e", "puddle-app"])):
    want = set(pk) - closure(roots)
    if listed(var) != want:
        bad.append(f"{var} is {sorted(listed(var))}; cargo metadata says {sorted(want)}")
dirs = {p["manifest_path"].split("/crates/")[-1].split("/")[0] for p in meta["packages"] if "/crates/" in p["manifest_path"]}
if dirs != set(pk):
    bad.append(f"crate directories {sorted(dirs)} differ from package names")
for b in bad:
    print("FAIL:", b, file=sys.stderr)
sys.exit(1 if bad else 0)
PY
    else
        bad "cargo metadata failed: $(cat "$tmp/meta.err")"
    fi
fi
