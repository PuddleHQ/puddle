// SPDX-License-Identifier: AGPL-3.0-or-later
// The state an upgrade must keep, made on the older build and checked on the newer one. ci/msi-install-test.ps1
// (-BaselineMsi) installs the MSIs and starts and stops the program; this script runs against the running,
// installed program, three ways:
//
//   node upgrade-state.mjs before <dir> <workspace>   on the older build, with the workspace running: makes a global
//       allow rule, a workspace deny rule, a pending request (a guest `curl` to a host nobody allowed), an
//       environment variable and a secret, three settings, an identity and a file in the workspace folder, then dumps every
//       GET the playbook names (upgrade-compare.mjs DUMPED) into <dir>.
//   node upgrade-state.mjs plant <breakage>            between the two, with the program quit: damages the database the way a
//       broken migration would (drop-table, empty-env, empty-pending), so a run can show the check fails. route-throws damages nothing
//       here: msi.yml builds the newer build's UI with the Activity screen throwing, for the screen check to catch.
//   node upgrade-state.mjs after <dir> <workspace>    on the newer build, after the workspace started: every dump
//       equal (ids, names, counts, values), the build strictly newer (health.version), api_version not older, the guest sees the variable, the secret's stand-in and the
//       kept file in /workspace, approving the kept pending request lets the guest's retry through (200), the browser editor's server
//       answers, and each main screen of the window loads with no error alert (UI Automation; needs PUDDLE_APP_PID).
//
// Environment: PUDDLE_INSTALL_DIR (default %LOCALAPPDATA%\Programs\Puddle). Plain Node 24, no packages. Exit 0 when
// every check passed, 1 otherwise; one `ok` or `FAIL` line per check, the way usable.mjs prints them.
import { spawn } from "node:child_process";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { Uia, until } from "./uia.mjs";
import { DUMPED, ERROR_TEXT, buildIsNewer, buildIsNotOlder, compareDump, compareVersions } from "./upgrade-compare.mjs";

const local = process.env.LOCALAPPDATA;
if (!local) throw new Error("LOCALAPPDATA is not set: this runs on Windows");
const installDir = process.env.PUDDLE_INSTALL_DIR ?? join(local, "Programs", "Puddle");
const dataDir = join(local, "puddle");
const [mode, ...args] = process.argv.slice(2);

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}
const note = (text) => console.log(`     ${text}`);

// What the run chose, shared by both phases (the names the guest and the dumps look for).
const GLOBAL_ALLOW = "kept-allow.example.net";
const WORKSPACE_DENY = "kept-deny.example.org";
const PENDING_HOST = "example.com";
const VARIABLE = "QA_MARK";
const VARIABLE_VALUE = "old-build";
const SECRET = "QA_SECRET";
// A file in the workspace folder lives on the workspace's volume, so it survives the sandbox being made again. The
// older build's folder is /workspaces/<id> (builds before the single folder) or /workspace; the newer build's is /workspace.
const KEPT_NAME = "kept-file.txt";
const NEW_FOLDER = "/workspace";
const IDENTITY = "Kept identity";
const ROUTES = ["Workspaces", "Inbox", "Rules", "Identities", "Activity", "Settings"];

// ---- the app's API and the guest ----------------------------------------------------------------

const conn = (() => {
  if (mode === "plant") return undefined;
  return JSON.parse(readFileSync(join(dataDir, "api.json"), "utf8"));
})();

/** The program's REST API, as its screens call it. */
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

/** A call that must answer with one of `ok`; the answer's body is the failure's detail. */
async function must(what, method, path, body, ok = [200, 201, 204]) {
  const reply = await api(method, path, body);
  if (!ok.includes(reply.status)) throw new Error(`${what}: ${method} ${path} answered ${reply.status}: ${reply.body.slice(0, 300)}`);
  return reply;
}

const msbEnv = {
  ...process.env,
  MSB_PATH: join(installDir, "runtime", "msb.exe"),
  MSB_HOME: join(dataDir, "msb"),
  MSB_CONFIG_PATH: join(dataDir, "msb", "config.json"),
};

/** A command in the guest, as the session user `agent` (the user a terminal opens as) or as root. */
function guest(id, script, { root = false, within = 120_000 } = {}) {
  const wrapper = root ? 'exec sh -c "$1"' : 'exec runuser -u agent -- sh -c "$1"';
  const child = spawn(msbEnv.MSB_PATH, ["exec", id, "--", "sh", "-c", wrapper, "_", script], { env: msbEnv, stdio: ["ignore", "pipe", "pipe"] });
  let text = "";
  child.stdout.on("data", (c) => (text += c));
  child.stderr.on("data", (c) => (text += c));
  const timer = setTimeout(() => child.kill(), within);
  return new Promise((resolve) =>
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ code, text: text.trim() });
    }),
  );
}

/** What a login shell of the session user prints for a variable: where Puddle writes it for the guest. */
const variableInGuest = async (id, name) =>
  (await guest(id, `env -u ${name} bash -ic 'printf "%s" "$${name}"' 2>/dev/null`)).text;

/** The HTTP status of a guest `curl` to a host, and its decision header. */
async function curlInGuest(id, host) {
  const r = await guest(id, `curl -sS -o /dev/null -D - -w 'HTTP:%{http_code}\\n' --max-time 40 https://${host}/ 2>&1`, { within: 90_000 });
  const status = /HTTP:(\d+)/.exec(r.text)?.[1];
  return { status: status ? Number(status) : undefined, pending: /x-puddle-decision:\s*pending/i.test(r.text), text: r.text };
}

async function workspaceNow(id) {
  return (await api("GET", "/api/workspaces")).json?.workspaces?.find((w) => w.id === id);
}

async function runningAndIdle(id, what) {
  let started = false;
  return until(
    `${what}: workspace ${id} running`,
    async () => {
      const w = await workspaceNow(id);
      if (!w) throw new Error(`workspace ${id} is gone`);
      if (w.status === "running" && !w.busy) return w;
      if (!started && !w.busy && ["created", "stopped"].includes(w.status)) {
        started = true;
        await api("POST", `/api/workspaces/${id}/start`);
      }
      return false;
    },
    240_000,
    2000,
  );
}

// ---- the dumps ----------------------------------------------------------------------------------

async function dumpEverything(dir, workspace) {
  const dumps = {};
  for (const path of [...DUMPED, `/api/workspaces/${workspace}/env`]) {
    const reply = await must(`dump ${path}`, "GET", path);
    dumps[path] = reply.json;
    writeFileSync(join(dir, `${path.slice(5).replace(/\//g, "_")}.json`), JSON.stringify(reply.json, null, 2));
  }
  return dumps;
}

const readDumps = (dir) => JSON.parse(readFileSync(join(dir, "dumps.json"), "utf8"));

// ---- before: make the state -----------------------------------------------------------------------

async function before(dir, workspace) {
  mkdirSync(dir, { recursive: true });
  await runningAndIdle(workspace, "before");
  const health = (await must("health", "GET", "/api/health")).json;
  note(`older build: version ${health.version}, api ${health.api_version}, ui build ${health.ui_build_id}`);

  await must("first run", "PUT", "/api/first-run", { completed: true });
  const allow = (await must("global allow rule", "POST", "/api/rules", { scope: { type: "global" }, pattern: GLOBAL_ALLOW, effect: "allow" })).json;
  const deny = (await must("workspace deny rule", "POST", "/api/rules", { scope: { type: "workspace", workspace }, pattern: WORKSPACE_DENY, effect: "deny" })).json;
  check("a global allow rule and a workspace deny rule exist", Boolean(allow?.id && deny?.id), `rules ${allow?.id} and ${deny?.id}`);

  await must("variable", "PUT", `/api/workspaces/${workspace}/env/${VARIABLE}`, { kind: "plain", value: VARIABLE_VALUE });
  const secretValue = `kept-secret-${crypto.randomUUID()}`;
  await must("secret", "PUT", `/api/workspaces/${workspace}/env/${SECRET}`, { kind: "secret", value: secretValue, hosts: ["example.net"] });
  check("an environment variable and a secret exist", true);

  const settings = (await must("settings", "GET", "/api/settings")).json;
  const changed = {
    workspace_defaults: { ...settings.workspace_defaults, memory: 3072 },
    vscode_server: settings.vscode_server,
    ui: { ...settings.ui, theme: "dark", close_behaviour: "quit" },
    tools: settings.tools,
    limits: settings.limits,
    revision: settings.revision,
  };
  await must("save settings", "PUT", "/api/settings", changed);
  const saved = (await must("settings read back", "GET", "/api/settings")).json;
  check("three settings changed (theme dark, memory 3072 MiB, closing quits)", saved.ui.theme === "dark" && saved.workspace_defaults.memory === 3072 && saved.ui.close_behaviour === "quit", JSON.stringify({ theme: saved.ui.theme, memory: saved.workspace_defaults.memory, close: saved.ui.close_behaviour }));

  const identity = (await must("identity", "POST", "/api/identities", { label: IDENTITY, author: { name: "Kept Author", email: "kept@example.com" }, credentials: [] })).json;
  check("an identity exists", Boolean(identity?.id), `identity ${identity?.id}`);

  const token = crypto.randomUUID();
  const wrote = await guest(workspace, `d=${NEW_FOLDER}; [ -d "$d" ] || d=/workspaces/${workspace}; printf '%s\\n' '${token}' > "$d/${KEPT_NAME}" && sync && cat "$d/${KEPT_NAME}"`);
  check("a file in the workspace folder is written", wrote.text === token, wrote.text.slice(0, 200));

  // The guest asks for a host nobody allowed: refused at once, and the request waits in the inbox.
  const asked = await curlInGuest(workspace, PENDING_HOST);
  check("the guest's request to an unknown host is refused as pending (a 403 to the proxy tunnel)", asked.pending && /\b403\b/.test(asked.text), asked.text.slice(0, 300));
  const open = await until(
    "the pending request in the inbox",
    async () => (await api("GET", "/api/pending")).json?.requests?.find((r) => r.state === "requested" && r.host === PENDING_HOST && r.workspace === workspace),
    60_000,
    1000,
  );
  check("one request is pending in the inbox", Boolean(open?.id), `request ${open?.id} for ${open?.host}`);

  const dumps = await dumpEverything(dir, workspace);
  writeFileSync(join(dir, "dumps.json"), JSON.stringify(dumps));
  writeFileSync(join(dir, "state.json"), JSON.stringify({ workspace, token, secretValue, pendingId: open.id, health }));
  check("every GET is dumped", Object.keys(dumps).length === DUMPED.length + 1, Object.keys(dumps).join(", "));
}

// ---- plant: damage the data like a broken migration -----------------------------------------------

function findStore(dir, depth = 0) {
  if (depth > 3) return undefined;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isFile() && entry.name === "puddle.db") return join(dir, entry.name);
    if (entry.isDirectory() && !["msb", "settings"].includes(entry.name)) {
      const found = findStore(join(dir, entry.name), depth + 1);
      if (found) return found;
    }
  }
  return undefined;
}

async function plant(breakage) {
  if (breakage === "route-throws") {
    console.log("route-throws is built into the newer build's UI (msi.yml), nothing to damage here");
    return;
  }
  const { DatabaseSync } = await import("node:sqlite");
  const file = findStore(dataDir);
  if (!file) throw new Error(`no puddle.db under ${dataDir}`);
  const db = new DatabaseSync(file);
  const sql = {
    "drop-table": "DROP TABLE rules",
    "empty-env": "DELETE FROM env_vars",
    "empty-pending": "UPDATE pending SET state = 'expired'",
  }[breakage];
  if (!sql) throw new Error(`unknown breakage ${breakage}: drop-table, empty-env, empty-pending or route-throws`);
  db.exec("PRAGMA foreign_keys = OFF");
  db.exec(sql);
  db.close();
  console.log(`planted ${breakage} in ${file}: ${sql}`);
}

// ---- after: check what the upgrade kept -------------------------------------------------------------

async function after(dir, workspace, appPid) {
  const state = JSON.parse(readFileSync(join(dir, "state.json"), "utf8"));
  const was = readDumps(dir);
  check("the program answers its API", (await api("GET", "/api/health")).status === 200);

  // S2.7c, S3.3c: every dump equal, before anything the newer build does with the workspace.
  const now = {};
  for (const path of Object.keys(was)) {
    const reply = await api("GET", path);
    if (reply.status === 200) now[path] = reply.json;
    else check(`GET ${path} answers`, false, `${reply.status}: ${reply.body.slice(0, 200)}`);
  }
  mkdirSync(join(dir, "after"), { recursive: true });
  writeFileSync(join(dir, "after", "dumps.json"), JSON.stringify(now));
  for (const path of Object.keys(was)) {
    let losses;
    try {
      losses = now[path] ? compareDump(path, was[path], now[path]) : [`GET ${path}: no answer`];
    } catch (err) {
      losses = [`GET ${path}: the answer has another shape (${err.message})`];
    }
    check(`GET ${path} equals what it was before the upgrade`, losses.length === 0, losses.slice(0, 5).join("; "));
  }

  // S4b: the build.
  const health = (await must("health", "GET", "/api/health")).json;
  note(`newer build: version ${health.version}, api ${health.api_version}, ui build ${health.ui_build_id} (was ${state.health.ui_build_id})`);
  const back = buildIsNotOlder(state.health, health);
  check("the build is not older (/api/health version and api_version)", back.length === 0, back.join("; "));
  const newer = buildIsNewer(state.health, health);
  check("the build is strictly newer (/api/health version)", newer.length === 0, newer.join("; "));
  check("the version is a version", !Number.isNaN(compareVersions(health.version, health.version)), health.version);

  // S3.4a, S3.5: the workspace starts, and the guest has what the older build gave it.
  const w = await runningAndIdle(workspace, "after");
  check("the workspace is running after the upgrade", w.status === "running");
  const mark = await variableInGuest(workspace, VARIABLE);
  check(`the guest sees ${VARIABLE}`, mark === VARIABLE_VALUE, JSON.stringify(mark));
  const stand = await variableInGuest(workspace, SECRET);
  check(`the guest sees a stand-in for ${SECRET}, never its value`, stand.length > 0 && stand !== state.secretValue && !stand.includes(state.secretValue), stand.length > 0 ? "a stand-in is set" : "empty");
  const home = await guest(workspace, `cat ${NEW_FOLDER}/${KEPT_NAME}`);
  check("the file in the workspace folder (/workspace) is there", home.text === state.token, home.text.slice(0, 200));

  // S3.6: the kept pending request, approved, lets the retry through.
  const pending = (await must("pending", "GET", "/api/pending")).json.requests.find((r) => r.id === state.pendingId);
  check("the pending request is still open with its id", pending?.state === "requested" && pending.host === PENDING_HOST, JSON.stringify(pending));
  await must("approve", "POST", `/api/pending/${state.pendingId}/approve`, {});
  const retried = await until(
    "the retry after the approval",
    async () => {
      const r = await curlInGuest(workspace, PENDING_HOST);
      return r.status === 200 ? r : r.status === undefined || r.status === 403 ? false : r;
    },
    90_000,
    4000,
  ).catch((err) => ({ status: undefined, text: err.message }));
  check("the guest's retry returns 200 after the kept request is approved", retried.status === 200, retried.text?.slice(0, 300));

  // S3.7: the browser editor.
  const attached = await api("POST", `/api/workspaces/${workspace}/attach`, { mode: "browser" });
  check("the browser editor opens", attached.status === 200 && attached.json?.opened === true, `${attached.status} ${attached.body.slice(0, 200)}`);
  const server = await until(
    "the editor's server in the guest",
    async () => {
      const r = await guest(workspace, "pgrep -af code-server | grep -o 'bind-addr 127.0.0.1:[0-9]*' | head -1", { root: true, within: 60_000 });
      return /127\.0\.0\.1:(\d+)/.exec(r.text)?.[1] ?? false;
    },
    240_000,
    3000,
  ).catch(() => undefined);
  check("the editor's server runs in the guest", server !== undefined, server ? `port ${server}` : "no code-server");
  if (server) {
    const health = await until(
      "the editor's health answer",
      async () => {
        const r = await guest(workspace, `curl -s -o /dev/null -w '%{http_code}' --max-time 10 http://127.0.0.1:${server}/healthz`, { root: true, within: 60_000 });
        return r.text === "200" ? r.text : false;
      },
      120_000,
      3000,
    ).catch(() => undefined);
    check("the editor's server answers 200 on /healthz", health === "200");
  }

  // S4c: every main screen loads with no error alert.
  if (appPid) await screens(appPid, dir, (await workspaceNow(workspace)).name);
  else check("the main screens load (needs PUDDLE_APP_PID)", false, "PUDDLE_APP_PID is not set");
}

async function screens(appPid, dir, workspaceName) {
  const MAIN = `^(?!${workspaceName} - )`; // the main window; the editor's window is "<name> - Puddle"
  const uia = await Uia.start(Number(appPid));
  try {
    for (const route of ROUTES) {
      try {
        await uia.click(`the ${route} link`, { window: MAIN, role: "link", nameRegex: `^${route}` });
        await uia.waitFor(`the ${route} heading`, { window: MAIN, localized: "heading", name: route }, 60_000);
        await sleep(1500);
        const alerts = await uia.find({ window: MAIN, localized: "alert" });
        const dump = await uia.dump(MAIN);
        const errorLine = dump.find((line) => ERROR_TEXT.test(line));
        writeFileSync(join(dir, `uia-tree-${route.toLowerCase()}.txt`), dump.join("\n"));
        check(`the ${route} screen loads with no error alert`, alerts.length === 0 && !errorLine, alerts.map((a) => a.name).join("; ") || errorLine || "");
      } catch (err) {
        check(`the ${route} screen loads with no error alert`, false, err.message);
      }
    }
  } finally {
    uia.close();
  }
}

// ---- the run ----------------------------------------------------------------------------------------

try {
  if (mode === "before") await before(args[0], args[1]);
  else if (mode === "after") await after(args[0], args[1], process.env.PUDDLE_APP_PID);
  else if (mode === "plant") await plant(args[0]);
  else throw new Error("usage: upgrade-state.mjs before <dir> <workspace> | plant <breakage> | after <dir> <workspace>");
} catch (err) {
  check(`${mode} ran to its end`, false, err.message);
}
if (failures.length > 0) {
  console.log(`\n${failures.length} check(s) failed:\n  - ${failures.join("\n  - ")}`);
  process.exit(1);
}
console.log(`\n${mode}: every check passed`);
process.exit(0);
