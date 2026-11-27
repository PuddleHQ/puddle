// SPDX-License-Identifier: AGPL-3.0-or-later
// The Windows variant of the soak (docs/qa/soak-8h.md, the Windows-only rows), with no person: the
// installed, release app on a windows-2025 runner with real virtual machines (WHP), played on a compressed
// day and measured by `puddle-soak sample` (handle counts, working sets and commit charge of the Puddle
// processes and the machine), then judged by `puddle-soak judge`.
//
// The day, in playbook minutes (WINSOAK_HOUR_SECS real seconds make one playbook hour; 1500 fits the 12
// playbook hours into 5 hours, 60 is the pull-request run):
//     0  start the app; two workspaces made and started; the sampler starts
//   a loop of guest commands in both workspaces every 30 minutes until hour 8
//    90  the editor window of the first workspace opened and closed 10 times in a row: the window is
//        there after each open and gone after each close
//   120  the tray's "Quit Puddle" through UI Automation (desktop.ps1 tray-quit): no `puddle-app`, `puddle` or
//        `msb` process within 90 s; a new start lists every workspace again (ids and names equal); both are
//        started again
//   480  the loops stop; the idle night (4 playbook hours): the sampler alone
//   720  the workspaces are still running; Get-WinEvent of Puddle's processes is collected
//        (collect-windows-events.ps1): an Application Error or hang of `puddle-app`, `puddle` or `msb` fails
//        the run; the judge reads metrics.tsv (handles per process flat over the idle hour, RSS, commit charge)
//
// Environment: WINSOAK_OUT (the run folder), WINSOAK_SOAK (puddle-soak.exe), WINSOAK_HOUR_SECS (default 1500),
// WINSOAK_PLANT (a breakage the run must catch, see PLANTS below: the run then passes only when exactly
// that check fails, which shows the check can fail). Plain Node 24, no packages.
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";
import { GiveUp, until } from "./poll.mjs";
import { onChildEnd } from "./child-end.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const local = process.env.LOCALAPPDATA;
if (!local) throw new Error("LOCALAPPDATA is not set: this runs on Windows");
const installDir = process.env.PUDDLE_INSTALL_DIR ?? join(local, "Programs", "Puddle");
const dataDir = join(local, "puddle");
const out = process.env.WINSOAK_OUT ?? join(tmpdir(), "puddle-soak-windows");
mkdirSync(out, { recursive: true });
const soakExe = process.env.WINSOAK_SOAK;
if (!soakExe) throw new Error("set WINSOAK_SOAK to puddle-soak.exe");
const HOUR = Number(process.env.WINSOAK_HOUR_SECS ?? 1500);
if (!(HOUR >= 30)) throw new Error("WINSOAK_HOUR_SECS must be at least 30");
const ACTIVE_HOURS = 8;
const IDLE_HOURS = 4;
const EDITOR_ROUNDS = 10;
const NAMES = ["qa-soak-win-a", "qa-soak-win-b"];

/** Breakages the run must catch: the one check that has to fail, by planted name. */
const PLANTS = {
  "lose-a-workspace": "a new start lists every workspace again",
  "keep-the-editor-open": "the editor window closes after each close",
};
const plant = process.env.WINSOAK_PLANT || "";
if (plant && !PLANTS[plant]) throw new Error(`unknown WINSOAK_PLANT ${plant}; known: ${Object.keys(PLANTS).join(", ")}`);

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}
function note(text) {
  console.log(`     ${text}`);
}
const real = (playbookMinutes) => Math.round((playbookMinutes * HOUR * 1000) / 60);
const t0 = Date.now();
/** Waits until playbook minute `minute` of the day (a real wait, paced by the clock, not a condition). */
async function waitUntilMinute(minute) {
  const left = t0 + real(minute) - Date.now();
  if (left > 0) await sleep(left);
}

// ---- the installed app ---------------------------------------------------------------------------

// The default guest is 8 GiB, and WHP commits all of it: two of them took 95 to 97 % of a 16 GB runner's
// commit limit from the first sample on (runs 45868035500, 45844667767), so the commit-charge line fired with
// no leak. The loops need little; the editor rounds (code-server) ran in 2 GiB in microsoft-editor.mjs.
const GUEST_MEMORY_MIB = 2048;
const connectionFile = join(dataDir, "api.json");
const appExe = join(installDir, "puddle-app.exe");
let app;
let appExited = false;
let conn;
function launchApp() {
  const child = spawn(appExe, [], { stdio: ["ignore", "inherit", "inherit"] });
  onChildEnd(child, (code) => {
    if (child === app) appExited = true;
    console.log(`puddle-app ${child.pid} exited (${code})`);
  });
  app = child;
  appExited = false;
}
async function connectToApp() {
  conn = undefined;
  await until(
    "the installed app's connection file",
    async () => {
      if (appExited) throw new GiveUp("puddle-app exited before it wrote its connection file");
      const read = JSON.parse(readFileSync(connectionFile, "utf8"));
      if (!read?.url || !read?.token) return false;
      conn = read;
      return (await api("GET", "/api/health")).status === 200;
    },
    180_000,
    1000,
  );
}
async function api(method, path, body) {
  const response = await fetch(`${conn.url}${path}`, {
    method,
    headers: { Authorization: `Bearer ${conn.token}`, "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(120_000),
  });
  const text = await response.text();
  let json;
  try {
    json = JSON.parse(text);
  } catch {
    json = undefined;
  }
  return { status: response.status, body: text, json };
}
const workspaces = async () => (await api("GET", "/api/workspaces")).json?.workspaces ?? [];

/** Starts every listed workspace that is not up and waits until each is running with nothing in hand. */
async function startAll(ids) {
  for (const id of ids) {
    const w = (await workspaces()).find((x) => x.id === id);
    if (w && w.status !== "running" && !w.busy) await api("POST", `/api/workspaces/${id}/start`);
  }
  for (const id of ids) {
    await until(
      `workspace ${id} to run`,
      async () => {
        const w = (await workspaces()).find((x) => x.id === id);
        if (!w) throw new GiveUp(`workspace ${id} vanished`);
        if (["crashed", "volume_missing"].includes(w.status)) throw new GiveUp(`workspace ${id} is ${w.status}`);
        return w.status === "running" && !w.busy;
      },
      900_000,
      3000,
    );
  }
}

/** Stops and deletes a workspace the way the screen does (stop, delete-check, delete with its fingerprint); the answer's status. */
async function deleteWorkspace(id) {
  await api("POST", `/api/workspaces/${id}/stop`);
  await until("the workspace to be stopped", async () => (await workspaces()).find((w) => w.id === id)?.status === "stopped", 180_000, 2000);
  const seen = await api("GET", `/api/workspaces/${id}/delete-check`);
  return (await api("DELETE", `/api/workspaces/${id}`, { confirm: true, fingerprint: seen.json?.fingerprint })).status;
}

// ---- the Windows shell (desktop.ps1) and the guests (the installed runtime's own msb) --------------

const desktopScript = join(here, "desktop.ps1");
function desktop(action, ...args) {
  const run = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", desktopScript, action, ...args], { encoding: "utf8", timeout: 120_000 });
  if (run.status !== 0) throw new Error(`desktop.ps1 ${action} failed (${run.status}): ${(run.stderr || run.stdout || String(run.error)).slice(0, 400)}`);
  return run.stdout.trim();
}
const puddleProcesses = () => JSON.parse(desktop("processes", "puddle-app", "puddle", "msb") || "[]");
const editorWindow = (name) => JSON.parse(desktop("window", String(app.pid), `^${name} - `, "state"));
const closeEditorWindow = (name) => JSON.parse(desktop("window", String(app.pid), `^${name} - `, "close"));

const msb = join(installDir, "runtime", "msb.exe");
const msbEnv = { ...process.env, MSB_PATH: msb, MSB_HOME: join(dataDir, "msb"), MSB_CONFIG_PATH: join(dataDir, "msb", "config.json") };
/** One short command in a guest; its exit code. */
function guest(id, script) {
  return spawnSync(msb, ["exec", id, "--", "sh", "-c", script], { env: msbEnv, encoding: "utf8", timeout: 120_000 }).status;
}

// ---- the sampler ----------------------------------------------------------------------------------

const metrics = join(out, "metrics.tsv");
let sampler;
/** (Re)starts `puddle-soak sample` against the app's current API: the token is new at every start. */
function startSampler() {
  stopSampler();
  const interval = Math.max(5, Math.round(HOUR / 12));
  const args = ["sample", "--out", metrics, "--api", new URL(conn.url).host, "--token", conn.token, "--interval", String(interval), "--disk", dataDir];
  for (const prefix of ["puddle-app", "puddle$", "msb"]) args.push("--proc", prefix);
  sampler = spawn(soakExe, args, { stdio: ["ignore", "inherit", "inherit"] });
  onChildEnd(sampler, (code) => note(`sampler ${sampler?.pid} ended (${code})`));
}
function stopSampler() {
  if (sampler) spawnSync("taskkill", ["/PID", String(sampler.pid), "/T", "/F"], { stdio: "ignore" });
  sampler = undefined;
}

// ---- the day ---------------------------------------------------------------------------------------

/** The editor window of `name`, opened and closed `rounds` times; the window shows after each open and is gone after each close. */
async function editorRounds(name, id, rounds) {
  let opened = 0;
  let closed = 0;
  for (let round = 1; round <= rounds; round++) {
    const reply = await api("POST", `/api/workspaces/${id}/attach`, { mode: "browser" });
    if (reply.status !== 200 || reply.json?.opened !== true) throw new Error(`attach ${round}: ${reply.status} ${reply.body.slice(0, 200)}`);
    const shown = await until(`the editor window of round ${round}`, () => editorWindow(name).found, 180_000, 2000).catch(() => false);
    if (shown) opened += 1;
    closeEditorWindow(name);
    const keepOpen = plant === "keep-the-editor-open" && round === 3;
    if (keepOpen) note(`planted: round ${round} leaves its window open`);
    const gone = keepOpen
      ? false
      : await until(`the editor window of round ${round} to close`, () => !editorWindow(name).found, 60_000, 1000).catch(() => false);
    if (gone) closed += 1;
    else if (keepOpen) closeEditorWindow(name); // the rest of the day starts clean
  }
  check("the editor window shows after each open", opened === rounds, `${opened} of ${rounds}`);
  check("the editor window closes after each close", closed === rounds, `${closed} of ${rounds}`);
}

/** The tray's Quit Puddle, then a new start: no Puddle process is left and every workspace is listed again. */
async function trayQuitAndRelaunch(before) {
  const started = Date.now();
  const chosen = desktop("tray-quit");
  const left = await until("no Puddle process left", () => (puddleProcesses().length === 0 ? [] : false), 90_000, 2000).catch(() => puddleProcesses());
  check(
    "the tray's Quit Puddle leaves no puddle-app.exe, puddle.exe or msb.exe process within 90 s",
    chosen === "quit chosen" && left.length === 0,
    `${chosen}; after ${Math.round((Date.now() - started) / 1000)} s: ${left.map((p) => `${p.name} ${p.id}`).join(", ") || "none"}`,
  );
  for (const p of left) spawnSync("taskkill", ["/PID", String(p.id), "/T", "/F"], { stdio: "ignore" });
  launchApp();
  await connectToApp();
  const after = await workspaces();
  const key = (list) => list.map((w) => `${w.id}:${w.name}`).sort().join(",");
  check("a new start lists every workspace again", key(after) === key(before), `before ${key(before)}; after ${key(after)}`);
}

function sampleFileHasWindowsReadings() {
  const text = existsSync(metrics) ? readFileSync(metrics, "utf8") : "";
  const has = (re) => re.test(text);
  check("the sampler wrote handle counts of the Puddle processes", has(/proc\.puddle-app\.\d+\.handles/));
  check("the sampler wrote the machine's commit charge and its limit", has(/host\.commit_kb/) && has(/host\.commit_limit_kb/));
}

function collectEvents(sinceIso) {
  const file = join(out, "windows-events.txt");
  const run = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", join(here, "collect-windows-events.ps1"), "-Since", sinceIso, "-Out", file], { encoding: "utf8", timeout: 300_000 });
  note((run.stdout ?? "").trim().split(/\r?\n/).slice(-3).join(" | "));
  check("Get-WinEvent of Puddle's processes is collected, with no crash or hang of them", run.status === 0 && existsSync(file), (run.stderr || "").slice(0, 300));
}

const startedIso = new Date().toISOString();
try {
  launchApp();
  await connectToApp();
  check("the installed app starts and answers its API", true, conn.url);
  const ids = [];
  for (const name of NAMES) {
    const made = await api("POST", "/api/workspaces", { name, memory_mib: GUEST_MEMORY_MIB });
    if (made.status >= 300) throw new Error(`creating ${name}: ${made.status} ${made.body.slice(0, 200)}`);
    ids.push(made.json?.id ?? (await workspaces()).find((w) => w.name === name)?.id);
  }
  await startAll(ids);
  check("both workspaces run", true, ids.join(", "));
  startSampler();

  // The loaded part: a guest command in each workspace every half hour; the events at their minutes.
  const loop = async (until_) => {
    for (let m = 30; m <= until_; m += 30) {
      await waitUntilMinute(m);
      for (const id of ids) guest(id, "date >> /tmp/soak-loop.log && ls / >/dev/null");
      if (m === 90) await editorRounds(NAMES[0], ids[0], EDITOR_ROUNDS);
      if (m === 120) {
        if (plant === "lose-a-workspace") {
          const gone = await deleteWorkspace(ids[1]);
          note(`planted: ${NAMES[1]} deleted before the quit (${gone}), the expected list keeps it`);
        }
        const before = [];
        for (const [i, id] of ids.entries()) before.push({ id, name: NAMES[i] });
        stopSampler();
        await trayQuitAndRelaunch(before);
        startSampler();
        await startAll((await workspaces()).map((w) => w.id));
      }
    }
  };
  await loop(ACTIVE_HOURS * 60);
  note("the idle night starts: the sampler alone");
  await waitUntilMinute((ACTIVE_HOURS + IDLE_HOURS) * 60);
  const still = await workspaces();
  check("no workspace stopped by itself during the night", still.filter((w) => NAMES.includes(w.name)).every((w) => w.status === "running"), still.map((w) => `${w.name} ${w.status}`).join(", "));
  stopSampler();
  sampleFileHasWindowsReadings();
  collectEvents(startedIso);
} catch (err) {
  check("the day ran to its end", false, String(err?.stack ?? err));
} finally {
  stopSampler();
  for (const p of (() => { try { return puddleProcesses(); } catch { return []; } })()) spawnSync("taskkill", ["/PID", String(p.id), "/T", "/F"], { stdio: "ignore" });
}

// ---- the judge -------------------------------------------------------------------------------------

const judged = spawnSync(soakExe, ["judge", metrics, "--hour-secs", String(HOUR), "--active-hours", String(ACTIVE_HOURS)], { encoding: "utf8" });
writeFileSync(join(out, "findings.txt"), judged.stdout ?? "");
console.log(judged.stdout);
// A pull-request run (a minute an hour) is too short and too stretched by its own events for the leak lines: it
// shows the path works and its findings are only printed.
if (HOUR >= 600) check("the judge finds no MAJOR in the series (handles flat over the idle hour, RSS, commit charge)", judged.status === 0, `exit ${judged.status}`);
else note(`short run: the judge's verdict (exit ${judged.status}) is not a check`);

if (plant) {
  const expected = PLANTS[plant];
  const exact = failures.length === 1 && failures[0] === expected;
  console.log(exact ? `PLANT ${plant}: the run failed on "${expected}" and on nothing else` : `PLANT ${plant}: wanted exactly "${expected}" to fail, failures were: ${failures.join(" | ") || "none"}`);
  process.exit(exact ? 0 : 1);
}
if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed:\n- ${failures.join("\n- ")}`);
  process.exit(1);
}
