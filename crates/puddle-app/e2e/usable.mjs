// SPDX-License-Identifier: GPL-3.0-or-later
// The "usable" run (install, create, approve, edit, build, push), with no person: the installed,
// release build of the app on Windows, driven through the REST API its own screens call, with real
// virtual machines and a real Git forge (the lab's, see below).
//
//   1. an SSH remote is refused with the explicit message;
//   2. a workspace is created from the lab forge's private repository (its root is in the machine's
//      certificate store, like a company root); every request the workspace raises is approved the way
//      the inbox does it, `POST /api/pending/{id}/approve`;
//   3. the browser editor opens in its own window (a top-level window titled "<name> - Puddle" appears)
//      and its server runs in the guest;
//   4. a real `apt-get install nodejs npm` (approved in the inbox) and a real `npm run build` and
//      `npm test` run in the workspace's checkout;
//   5. `git push` from the workspace reaches the forge with the credential Puddle injects, the author
//      is the identity's, and the guest, the audit log and the API never hold the token.
//
// Why UI Automation (uia.mjs) and not the DevTools Protocol like smoke.mjs: a release build has no
// debugging port (the shell honours PUDDLE_APP_DEBUG_PORT in debug builds only, and WebView2 ignores its
// environment variable once the API sets arguments, which wry always does), and the MSI holds a release
// build. So the real window is clicked the way a person does it: the first-run steps, the create form,
// the inbox's Allow, the Connect step and the editor window's content. The REST API still sets up what a
// person would paste (the token, the identity), asks for what no screen shows (the ids, the guest) and
// retries the clone. Plain Node 24, no packages.
//
// The lab forge runs as a child of this script, not of an earlier workflow step: a step's background
// processes do not outlive it on the runner. Its root goes into the machine's store before the app starts
// (the app reads the store then), like a company root.
//
// Environment: USABLE_LAB_BIN (`puddle-lab-forgejo`; PUDDLE_FORGEJO_BIN names the forge program it runs),
// USABLE_FORGE_NAME (a name the hosts file points at this machine), PUDDLE_INSTALL_DIR (default
// %LOCALAPPDATA%\Programs\Puddle), USABLE_OUT (a folder for screenshots and logs; default a temp one).
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { Uia } from "./uia.mjs";
import { onChildEnd } from "./child-end.mjs";

const local = process.env.LOCALAPPDATA;
if (!local) throw new Error("LOCALAPPDATA is not set: this runs on Windows");
const installDir = process.env.PUDDLE_INSTALL_DIR ?? join(local, "Programs", "Puddle");
const dataDir = join(local, "puddle");
const out = process.env.USABLE_OUT ?? join(tmpdir(), "puddle-usable");
mkdirSync(out, { recursive: true });
if (!process.env.USABLE_LAB_BIN || !process.env.USABLE_FORGE_NAME) throw new Error("set USABLE_LAB_BIN and USABLE_FORGE_NAME");
const forge = process.env.USABLE_FORGE_NAME;
// The lab forge's account and repository (crates/puddle-e2e/src/bin/puddle-lab-forgejo.rs).
const httpsUrl = `https://${forge}/acme/app.git`;
const sshUrl = `git@${forge}:acme/app.git`;
const WORKSPACE = "usable";

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}
function note(text) {
  console.log(`     ${text}`);
}

/** Polls `probe` until it is truthy, for `within` ms; the reason is named at each call, never a fixed sleep. */
async function until(what, probe, within = 60_000, every = 500) {
  const deadline = Date.now() + within;
  for (;;) {
    const value = await Promise.resolve().then(probe).catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out after ${Math.round(within / 1000)} s waiting for ${what}`);
    await sleep(every);
  }
}

// ---- the lab forge ----------------------------------------------------------------------------------

const infoFile = join(out, "forgejo.json");
const stopFile = join(out, "forgejo.stop");
const lab = spawn(process.env.USABLE_LAB_BIN, ["--name", forge, "--port", "443", "--info", infoFile, "--stop", stopFile], { stdio: ["ignore", "ignore", "pipe"] });
let labLog = "";
lab.stderr.on("data", (c) => (labLog += c));
let labExited = false;
onChildEnd(lab, () => (labExited = true));
let info;
let rootStored = false;
async function startForge() {
  info = await until(
    "the lab forge's info file",
    () => {
      if (labExited) throw new Error(`the lab forge exited: ${labLog}`);
      return JSON.parse(readFileSync(infoFile, "utf8"));
    },
    180_000,
    500,
  );
  // LocalMachine\Root takes no dialog (the runner is an administrator).
  const added = spawnSync("certutil", ["-addstore", "-f", "Root", info.root_pem], { encoding: "utf8" });
  rootStored = added.status === 0;
  note(`lab root in the machine's store: ${rootStored} ${rootStored ? "" : added.stdout}`);
  if (!rootStored) throw new Error("the lab root could not be added to the store");
}
function stopForge() {
  if (!info) return;
  writeFileSync(stopFile, "stop");
  if (rootStored) spawnSync("certutil", ["-delstore", "Root", "Puddle lab Forgejo root"], { stdio: "ignore" });
}

// ---- the installed app ------------------------------------------------------------------------------

await startForge().catch((err) => {
  console.error(`FAIL the lab forge did not start: ${err}`);
  process.exit(1);
});

const connectionFile = join(dataDir, "api.json");
const app = spawn(join(installDir, "puddle-app.exe"), [], { stdio: ["ignore", "inherit", "pipe"] });
let appLog = "";
app.stderr.on("data", (chunk) => {
  appLog += chunk;
  process.stderr.write(chunk);
});
let appExited = false;
onChildEnd(app, (code) => {
  appExited = true;
  console.log(`puddle-app exited (${code})`);
});

let conn;
let uia;
/** The app's REST API, as its screens call it. */
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

// ---- the guest, through the installed runtime's own msb -----------------------------------------

const msbEnv = {
  ...process.env,
  MSB_PATH: join(installDir, "runtime", "msb.exe"),
  MSB_HOME: join(dataDir, "msb"),
  MSB_CONFIG_PATH: join(dataDir, "msb", "config.json"),
};

/** A command in the guest, as root or as the session user `agent` (the user a terminal opens as). */
function guest(id, script, { root = false, within = 600_000 } = {}) {
  const wrapper = root
    ? 'exec sh -c "$1"'
    : 'if id agent >/dev/null 2>&1; then exec runuser -u agent -- sh -c "$1"; else exec sh -c "$1"; fi';
  const child = spawn(msbEnv.MSB_PATH, ["exec", id, "--", "sh", "-c", wrapper, "_", script], { env: msbEnv, stdio: ["ignore", "pipe", "pipe"] });
  let text = "";
  child.stdout.on("data", (c) => (text += c));
  child.stderr.on("data", (c) => (text += c));
  const timer = setTimeout(() => child.kill(), within);
  return new Promise((resolve) =>
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ code, text });
    }),
  );
}

// ---- the inbox ------------------------------------------------------------------------------------

const approved = [];

/** The hosts this run's workspace is meant to reach: the lab forge and the distribution's package archive. */
const expectedHost = (host) => host === forge || /(^|\.)debian\.org$/.test(host) || /fastly/.test(host);

/** Approves the expected pending requests, as the inbox's Allow does (others stay for the user); how many. */
async function approvePending(what) {
  const pending = (await api("GET", "/api/pending")).json?.requests ?? [];
  const open = pending.filter((r) => r.state === "requested");
  for (const row of open.filter((r) => !expectedHost(r.host))) {
    if (!approved.includes(`left: ${row.host}`)) {
      approved.push(`left: ${row.host}`);
      note(`not approved (not a host of this run): ${row.host} for ${row.workspace}`);
    }
  }
  for (const row of open.filter((r) => expectedHost(r.host))) {
    const reply = await api("POST", `/api/pending/${row.id}/approve`, {});
    approved.push(`${row.host} for ${row.workspace}`);
    note(`approved in the inbox (${what}): ${row.host} for ${row.workspace} -> ${reply.status}`);
  }
  return open.filter((r) => expectedHost(r.host)).length;
}

/**
 * A command in the guest that reaches hosts nobody has allowed yet. A request that waits for approval
 * is refused to the tool at once (a 403), so the attempt fails; the user approves it in the inbox and
 * runs the command again, as here, until it passes or the tries are used.
 */
async function runApproving(what, id, script, { root = false, ok, tries = 8, within = 600_000 }) {
  let last;
  for (let attempt = 1; attempt <= tries; attempt++) {
    last = await guest(id, script, { root, within });
    if (ok(last)) return { ...last, attempts: attempt };
    const until = Date.now() + 10_000;
    while (Date.now() < until && (await approvePending(what)) === 0) await sleep(1000);
  }
  return { ...last, attempts: tries };
}

// ---- windows and the screen -----------------------------------------------------------------------

const windowScript = join(out, "windows.ps1");
writeFileSync(
  windowScript,
  `Add-Type @"
using System; using System.Collections.Generic; using System.Text; using System.Runtime.InteropServices;
public class Wins {
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] static extern bool EnumWindows(EnumProc p, IntPtr l);
  [DllImport("user32.dll")] static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] static extern bool IsWindowVisible(IntPtr h);
  public static List<string> All() {
    var l = new List<string>();
    EnumWindows((h, _) => { uint pid; GetWindowThreadProcessId(h, out pid); var s = new StringBuilder(512); GetWindowText(h, s, 512);
      if (IsWindowVisible(h) && s.Length > 0) l.Add(pid + "\\t" + s); return true; }, IntPtr.Zero);
    return l;
  }
}
"@
[Wins]::All()
`,
);
/** The titles of the visible top-level windows of the app. */
function appWindows() {
  const ps = spawnSync("powershell", ["-NoProfile", "-File", windowScript], { encoding: "utf8" });
  return (ps.stdout ?? "")
    .split(/\r?\n/)
    .map((line) => line.split("\t"))
    .filter(([pid, title]) => Number(pid) === app.pid && title)
    .map(([, title]) => title);
}

let shots = 0;
function screenshot(name) {
  const file = join(out, `${String(++shots).padStart(2, "0")}-${name}.png`);
  spawnSync(
    "powershell",
    [
      "-NoProfile",
      "-Command",
      `Add-Type -AssemblyName System.Windows.Forms,System.Drawing; $b = [System.Windows.Forms.SystemInformation]::VirtualScreen; ` +
        `$bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height; $g = [System.Drawing.Graphics]::FromImage($bmp); ` +
        `$g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size); $bmp.Save('${file}')`,
    ],
    { stdio: "ignore" },
  );
}

/** The tail of the lab forge's own log, for a step that cannot reach it. */
function forgeLog() {
  try {
    return readFileSync(join(dirname(info.root_pem), "forgejo.log"), "utf8").split("\n").slice(-15).join(" | ").slice(-900);
  } catch (err) {
    return `no forge log: ${err.message}`;
  }
}

/** Waits until the workspace has nothing in hand (a clone, a start): other calls refuse while it is busy. */
const idle = (id) =>
  until(
    `workspace ${id} to be idle`,
    async () => {
      const w = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.id === id);
      return w && !w.busy ? w : false;
    },
    300_000,
    2000,
  );

// ---- the real window ------------------------------------------------------------------------------

const MAIN = "^(?!usable - )"; // the app's main window; the editor's window is "<name> - Puddle"
const EDITOR = `^${WORKSPACE} - `;
const inMain = (role, name) => ({ window: MAIN, role, name });
const heading = (name) => ({ window: MAIN, localized: "heading", name });
let dumps = 0;

/** One thing a person does in the window; a failure leaves the window's tree and a screenshot behind. */
async function step(what, action) {
  try {
    const result = await action();
    check(what, true);
    return result;
  } catch (err) {
    const lines = await uia.dump(".").catch((e) => [`dump failed: ${e}`]);
    writeFileSync(join(out, `uia-tree-${String(++dumps).padStart(2, "0")}.txt`), lines.join("\n"));
    screenshot("uia-failure");
    throw new Error(`${what}: ${err.message}`);
  }
}

/** The first-run flow, step by step, ending with the create form on the last step. */
async function firstRunToCreateForm() {
  await uia.waitFor("the welcome heading", heading("Welcome to Puddle"), 120_000);
  await uia.click("Get started", inMain("link", "Get started"));
  const pages = ["System check", "Certificates", "How do you want to connect?", "Which hosts should Puddle decrypt?", "Look", "Your first workspace"];
  await uia.waitFor(`the "${pages[0]}" heading`, heading(pages[0]));
  for (const page of pages.slice(1)) {
    if (page === "Look") {
      // The interception step saves its choice with a button, not a link: the person keeps the recommended mode.
      await uia.click(`Continue (to ${page})`, inMain("button", "Continue"));
    } else {
      await uia.click(`Continue (to ${page})`, inMain("link", "Continue"));
    }
    await uia.waitFor(`the "${page}" heading`, heading(page));
    if (page === "Which hosts should Puddle decrypt?") {
      const mode = await uia.waitFor("the recommended interception mode", { window: MAIN, role: "radio", nameRegex: "except a known list" });
      check("the recommended interception mode is preselected", mode.checked === true, JSON.stringify(mode));
    }
    if (page === "How do you want to connect?") {
      const code = await uia.waitFor("the bundled code-server choice", { window: MAIN, role: "radio", nameRegex: "code-server \\(bundled\\)" });
      check("code-server is preselected on the Connect step", code.checked === true, JSON.stringify(code));
    }
  }
}

/** Fills the New workspace form as a person does and presses Create. */
async function createFromTheForm() {
  await uia.click("Create a workspace", inMain("button", "Create a workspace"));
  await uia.waitFor("the New workspace dialog", { window: MAIN, localized: "dialog", name: "New workspace" });
  await uia.fill("the repository address", { window: MAIN, role: "edit", nameRegex: "^Git repository \\(HTTPS" }, httpsUrl);
  await uia.fill("the name", { window: MAIN, role: "edit", name: "Name" }, WORKSPACE);
  await uia.click("Create workspace", inMain("button", "Create workspace"));
}

// ---- the run ---------------------------------------------------------------------------------------

try {
  await until(
    "the installed app's connection file",
    () => {
      if (appExited) throw new Error("puddle-app exited before it wrote its connection file");
      conn = JSON.parse(readFileSync(connectionFile, "utf8"));
      return conn?.url && conn?.token;
    },
    180_000,
    1000,
  );
  await until("the API", async () => (await api("GET", "/api/health")).status === 200, 60_000);
  check("the installed app starts and answers its API", true, conn.url);
  uia = await Uia.start(app.pid);
  // Windows asks once to trust the app's own localhost certificate (the editor's origin), whenever the
  // app first needs it; the user answers Yes, so a poll does, for the whole run.
  const certificate = setInterval(() => {
    uia.call({ action: "click", window: "^Security Warning$", role: "pane", name: "Yes" }).then((r) => {
      if (r.found) note("the Windows certificate dialog for localhost: answered Yes");
    }, () => {});
  }, 2000);
  certificate.unref();

  // What the settings screen and the identity forms do through the same API; the forge is on this
  // computer, so a workspace may reach this computer's own addresses (the Loopback switch).
  const toggled = await api("PUT", "/api/settings", { workspace_defaults: { local_toggles: { loopback: true } } });
  check("the 'this computer' switch is on for new workspaces", toggled.status === 200, `${toggled.status} ${toggled.body.slice(0, 200)}`);
  const stored = await api("POST", "/api/credentials/stored", { host: forge, token: info.token });
  check("the access token is pasted and kept, never echoed", stored.status === 201 && !stored.body.includes(info.token), `${stored.status}`);
  const identity = await api("POST", "/api/identities", {
    label: "Lab",
    author: { name: "Usable Run", email: "usable@example.org" },
    credentials: [{ host: forge, source: stored.json.source, covers: { owners: [info.owner], rest_of_host: false } }],
  });
  check("an identity covers the forge's account", identity.status === 201, `${identity.status} ${identity.body.slice(0, 200)}`);

  // 1. An SSH remote is refused with the explicit message, and nothing is made.
  const ssh = await api("POST", "/api/workspaces", { name: "ssh-refused", repositories: [{ url: sshUrl }] });
  check("an SSH remote is refused with the explicit message", ssh.status >= 400 && ssh.status < 500 && /ssh remotes are not supported yet/.test(ssh.body), `${ssh.status} ${ssh.body.slice(0, 200)}`);
  const none = await api("GET", "/api/workspaces");
  check("the refused create made no workspace", none.json?.workspaces?.length === 0, none.body.slice(0, 200));

  // 2. The real create, through the window: the first-run steps, then the New workspace form.
  await step("the first-run steps lead to the first workspace's form", firstRunToCreateForm);
  await step("the New workspace form is filled and sent", createFromTheForm);
  const created = await until("the workspace to be listed", async () => (await api("GET", "/api/workspaces")).json?.workspaces?.find((w) => w.name === WORKSPACE), 60_000, 1000);
  const id = created.id;
  note(`workspace ${WORKSPACE} has the id ${id}`);
  let started = false;
  const running = await until(
    `workspace ${id} to run (the create clones the private repository, then the image is pulled and the machine boots)`,
    async () => {
      const w = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.id === id);
      if (!w) throw new Error("the workspace vanished");
      if (["crashed", "volume_missing"].includes(w.status)) throw new Error(`the workspace is ${w.status}`);
      if (w.status === "running" && !w.busy) return w;
      if (!started && !w.busy && ["created", "stopped"].includes(w.status)) {
        started = true;
        await api("POST", `/api/workspaces/${id}/start`);
      }
      return false;
    },
    900_000,
    2000,
  );
  check("a workspace made from the lab forge's private repository is running", running.status === "running", `${running.status}, image ${running.image}`);
  // The create allowed the forge itself before the first clone, so nobody approves it and nobody
  // clones again: the checkout is there and the rule says who made it.
  const checkout = `/workspaces/${id}/app/package.json`;
  const rules = (await api("GET", "/api/rules")).json?.rules ?? [];
  const grant = rules.find((r) => r.pattern === forge && r.effect === "allow" && r.scope?.type === "workspace" && r.scope.workspace === id);
  check("the create made an allow rule for the forge, marked as made by the create", grant?.created_by === "workspace_create", JSON.stringify(grant ?? rules));
  const cloned = (await guest(id, `test -f ${checkout} && echo present`, { root: true, within: 60_000 })).text.includes("present");
  check("the private repository is cloned into the workspace with the injected credential", cloned, checkout);
  const settled = await idle(id).catch(() => undefined);
  check("the workspace is idle and running after the clone", settled?.status === "running", JSON.stringify(settled));

  // 3. The browser editor opens in its own window, from the Connect step of the workspace list.
  await step("the Connect step opens the browser editor", async () => {
    await uia.click("the Workspaces link", { window: MAIN, role: "link", nameRegex: "^Workspaces" });
    await uia.click(`Connect to ${WORKSPACE}`, inMain("button", `Connect to ${WORKSPACE}`));
    await uia.waitFor("the Connect step", { window: MAIN, localized: "dialog", name: `Connect to ${WORKSPACE}` });
    await uia.click("Open in the browser", inMain("button", "Open in the browser"));
  });
  const title = await until(
    "the editor window, titled with the workspace",
    () => {
      return appWindows().find((t) => t.startsWith(`${WORKSPACE} - `));
    },
    120_000,
    1500,
  ).catch(() => undefined);
  check("the editor opens in its own window", title !== undefined, `windows: ${appWindows().join(" | ")}`);
  // What the window shows: the workbench's Explorer with the checkout's folder in it.
  await step("the editor window shows the Explorer with the checkout's folder", async () => {
    await uia.waitFor("the Files Explorer tree", { window: EDITOR, role: "tree", name: "Files Explorer" }, 180_000);
    await uia.waitFor("the checkout's folder in the Explorer", { window: EDITOR, role: "treeitem", name: "app" }, 60_000);
  }).catch((err) => {
    check("the editor window shows the Explorer with the checkout's folder", false, err.message);
  });
  const server = await until(
    "the editor server in the guest",
    async () => {
      const r = await guest(id, "pgrep -af code-server | head -2", { root: true, within: 60_000 });
      return r.text.includes("code-server") ? r.text : false;
    },
    240_000,
    3000,
  ).catch(() => undefined);
  check("the editor's server runs in the guest", server !== undefined, (server ?? "").slice(0, 200));
  screenshot("editor");

  // 4. A real build. Node and npm come from the distribution's own archive (an apt mirror the user approves).
  const aptDone = await runApproving(
    "apt",
    id,
    "export DEBIAN_FRONTEND=noninteractive; apt-get update -qq >/tmp/apt.log 2>&1 && apt-get install -y -qq --no-install-recommends nodejs npm >>/tmp/apt.log 2>&1; echo apt=$?; tail -3 /tmp/apt.log",
    { root: true, within: 900_000, ok: (r) => /apt=0/.test(r.text) },
  );
  check("nodejs and npm install from the distribution's archive after the approvals", /apt=0/.test(aptDone.text), `after ${aptDone.attempts} tries: ${aptDone.text.slice(-300)}`);
  const rootDir = (await guest(id, "find /workspaces -maxdepth 3 -name package.json -not -path '*/node_modules/*' | head -1 | xargs dirname")).text.trim().split("\n").pop();
  check("the checkout is in the workspace", rootDir.startsWith("/workspaces/"), rootDir);
  const build = await guest(id, `cd ${rootDir} && node --version && npm --version && npm install --no-audit --no-fund 2>&1 | tail -2 && npm run build 2>&1 && npm test 2>&1 | tail -8 && node dist/app.js`);
  check("npm run build and npm test pass in the workspace", build.code === 0 && /build ok/.test(build.text) && /hello from usable/.test(build.text), build.text.slice(-500));

  // 5. Push with the injected credential.
  const branch = `usable-${Date.now()}`;
  const push = await guest(
    id,
    `set -eu; cd ${rootDir}; git checkout -q -b ${branch}; git add -f dist/build-stamp.txt; git commit -q -m "build from the usable run"; git push -q origin ${branch} 2>&1; git log -1 --format=%ae; git rev-parse HEAD`,
  );
  const lines = push.text.trim().split("\n");
  const [author, head] = [lines.at(-2), lines.at(-1)];
  check("git push from the workspace succeeds", push.code === 0 && /^[0-9a-f]{40}$/.test(head ?? ""), push.text.slice(-400));
  check("the commit carries the identity's author", author === "usable@example.org", author);
  const asked = spawnSync(
    "curl.exe",
    ["--silent", "--show-error", "--ssl-no-revoke", "--cacert", info.root_pem, "--resolve", `${forge}:${info.port}:127.0.0.1`, "-H", `Authorization: token ${info.token}`, `https://${forge}:${info.port}/api/v1/repos/${info.owner}/${info.repo}/branches/${branch}`],
    { encoding: "utf8" },
  );
  let forgeHead;
  try {
    forgeHead = JSON.parse(asked.stdout).commit?.id;
  } catch {
    forgeHead = undefined;
  }
  check("the push reached the forge at the same commit", forgeHead === head, `${forgeHead}${forgeHead === head ? "" : ` curl: ${asked.stderr.slice(0, 200)} ${asked.stdout.slice(0, 200)} forge log: ${forgeLog()}`}`);

  // The token is nowhere a guest, the API or the audit log could show it. The control comes first: a
  // token planted in a file and in a process's environment must be found, or the search proves nothing.
  const [first, second] = [info.token.slice(0, 20), info.token.slice(20)];
  const search =
    `w="${first}""${second}"; printf '%s\\n' "$w" "$(printf 'x-access-token:%s' "$w" | base64 -w0)" "$(printf '%s' "$w" | base64 -w0)" > /dev/shm/.p; ` +
    `echo procs=$(cat /proc/[0-9]*/environ /proc/[0-9]*/cmdline 2>/dev/null | tr '\\0' '\\n' | grep -cFf /dev/shm/.p); ` +
    `echo files=$(grep -rlaFf /dev/shm/.p --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev --exclude=.p / 2>/dev/null | grep -v '^/dev/shm/.p$' | tr '\\n' ' '); rm -f /dev/shm/.p; echo done`;
  const planted = await guest(id, `w="${first}""${second}"; printf '%s\\n' "$w" > /root/planted.txt; env PLANTED="$w" sleep 120 & pid=$!; sleep 1; ${search}; kill $pid; rm -f /root/planted.txt`, { root: true });
  check("the token search finds a planted token (the control)", !/procs=0/.test(planted.text) && planted.text.includes("/root/planted.txt"), planted.text.replace(info.token, "<token>").slice(-300));
  const found = await guest(id, search, { root: true });
  check("the guest holds no token in a process or a file", /procs=0/.test(found.text) && /files=\s*\n?done/.test(found.text), found.text.slice(-300));
  const audit = await api("GET", `/api/audit?workspace=${id}&type=connection&limit=200`);
  check("the audit log holds no token and says the connections were injected", audit.status === 200 && !audit.body.includes(info.token) && audit.body.includes('"injected":true'), `${audit.status}`);
  const listing = await api("GET", "/api/identities");
  check("the API never gives the token back", !listing.body.includes(info.token) && !stored.body.includes(info.token));
  check("the app's log holds no token", !appLog.includes(info.token));
  check("requests were approved in the inbox", approved.length > 0, approved.join("; "));

  // Stop and delete, as the user does at the end.
  await api("POST", `/api/workspaces/${id}/stop`);
  await until("the workspace to be stopped", async () => (await api("GET", "/api/workspaces")).json?.workspaces?.find((w) => w.id === id)?.status === "stopped", 180_000, 2000).catch(() => {});
  const seen = await api("GET", `/api/workspaces/${id}/delete-check`);
  const gone = await api("DELETE", `/api/workspaces/${id}`, { confirm: true, fingerprint: seen.json?.fingerprint });
  check("the workspace is deleted", [200, 202, 204].includes(gone.status), `${gone.status} ${gone.body.slice(0, 200)}`);
  await until("the workspace to be gone", async () => !(await api("GET", "/api/workspaces")).json?.workspaces?.some((w) => w.id === id), 180_000, 2000).catch((err) => check("the workspace goes away", false, String(err)));
} catch (err) {
  check("the usable run ran to the end", false, String(err?.stack ?? err));
  note(`windows at the end: ${appWindows().join(" | ") || "none"}`);
  screenshot("failure");
} finally {
  writeFileSync(join(out, "app.log"), appLog);
  if (info) writeFileSync(join(out, "forgejo-server.log"), forgeLog());
  uia?.close();
  stopForge();
  if (!appExited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\nthe usable run passed");
process.exit(0);
