// SPDX-License-Identifier: AGPL-3.0-or-later
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
//      is the identity's, and the guest, the audit log and the API never hold the token;
//   6. the first run's "Copy report" puts the system check's report on the Windows clipboard;
//   7. the workspace's Terminal tab opens a shell: `whoami; pwd; sudo -n true && echo sudo-ok`, typed
//      with real keys, reads `agent`, the start folder and `sudo-ok`, and no error page shows; the page
//      itself is read over DevTools (terminal-render.mjs): emulator sized, stylesheet applied, terminal
//      font, ANSI colours unlike the default text, a mouse drag selects, no CSP or stylesheet error;
//   8. the close button hides the window in the tray, with exactly one Windows notification however
//      often it is pressed, and a heartbeat the guest writes every half second has no gap over 2 s
//      while the window is hidden;
//   9. a new request makes no notification while the window is in front, and exactly one, naming the
//      workspace and the host, while it is minimized;
//  10. Activity's "Export JSON lines" saves a .jsonl file in Downloads, one valid JSON record a line;
//  11. the close button still hides the window when the page failed to load (broken-page.mjs refuses
//      every request and reloads, so the window shows the browser's error page);
//  12. the tray's "Quit Puddle" with the workspace running leaves no `puddle-app`, `puddle` or `msb` process
//      within 90 s, and a new start lists the workspace as stopped.
// Steps 7 to 12 run in the direct run only (below): the shell does not change behind a proxy.
//
// Windows notifications are read from the Action Center's history for the app's AppUserModelID (the
// Start menu shortcut's, the `identifier` in tauri.conf.json), and the window's buttons are pressed with
// the messages its title bar sends (desktop.ps1).
//
// Why UI Automation (uia.mjs) and not the DevTools Protocol like smoke.mjs (the Terminal tab's rendering
// check alone uses a DevTools port, opened by a WebView2 policy value, see terminal-render.mjs): a release build has no
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
// Behind the company network: with USABLE_COMPANY_LAB_BIN set (`puddle-lab`, and PUDDLE_MITMDUMP_BIN for
// it) the same run happens on a machine whose only way out is the lab's company proxy, set up the way a
// company sets up a Windows computer: a PAC file as the user's automatic proxy configuration script (the
// registry value Windows settings write), and one proxy behind it that wants the Windows sign-in (NTLM,
// answered through SSPI as the signed-in user) and inspects every TLS connection with a root of its own,
// which sits in the machine's certificate store. Squid has no Windows build, so this proxy is the lab's
// NTLM-demanding interceptor (puddle-lab --interceptor-ntlm). The same checks run, plus checks that the
// workspace's traffic went through that proxy: the app's network-health report names the PAC, the NTLM
// sign-in and the company root, and the proxy's own log holds the sign-in and the decrypted Git requests.
//
// Environment: USABLE_LAB_BIN (`puddle-lab-forgejo`; PUDDLE_FORGEJO_BIN names the forge program it runs),
// USABLE_FORGE_NAME (a name the hosts file points at this machine), PUDDLE_INSTALL_DIR (default
// %LOCALAPPDATA%\Programs\Puddle), USABLE_OUT (a folder for screenshots and logs; default a temp one).
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { Uia } from "./uia.mjs";
import { onChildEnd } from "./child-end.mjs";
import { GiveUp, until } from "./poll.mjs";
import { debugPortPolicy, inspectTerminal } from "./terminal-render.mjs";
import { breakThePage } from "./broken-page.mjs";

const here = dirname(fileURLToPath(import.meta.url));
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

const companyLabBin = process.env.USABLE_COMPANY_LAB_BIN;
const company = Boolean(companyLabBin);
const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}
function note(text) {
  console.log(`     ${text}`);
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
      if (labExited) throw new GiveUp(`the lab forge exited: ${labLog}`);
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

// ---- the company network (company mode only) ---------------------------------------------------------

const companyInfoFile = join(out, "company-lab.json");
const companyStopFile = join(out, "company-lab.stop");
const INTERNET_SETTINGS = String.raw`HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings`;
let companyLab;
let companyLabLog = "";
let companyLabExited = false;
let companyInfo;
let companyRootStored = false;
let pacSet = false;
/**
 * The lab's company proxy, its root in the machine's store and its PAC file as the user's automatic
 * configuration script, all before the app starts (it reads the store and the settings then).
 */
async function startCompanyNetwork() {
  // The proxy resolves the forge's name through the machine's hosts file, like the app.
  companyLab = spawn(companyLabBin, ["--interceptor-ntlm", "--pac", "--info", companyInfoFile, "--stop", companyStopFile], { stdio: ["ignore", "ignore", "pipe"], env: process.env });
  companyLab.stderr.on("data", (c) => (companyLabLog += c));
  onChildEnd(companyLab, () => (companyLabExited = true));
  companyInfo = await until(
    "the company lab's info file",
    () => {
      if (companyLabExited) throw new GiveUp(`the company lab exited: ${companyLabLog}`);
      return JSON.parse(readFileSync(companyInfoFile, "utf8"));
    },
    180_000,
    500,
  );
  const added = spawnSync("certutil", ["-addstore", "-f", "Root", companyInfo.interceptor.ca], { encoding: "utf8" });
  companyRootStored = added.status === 0;
  if (!companyRootStored) throw new Error(`the company root could not be added to the store: ${added.stdout}`);
  const set = spawnSync("reg", ["add", INTERNET_SETTINGS, "/v", "AutoConfigURL", "/t", "REG_SZ", "/d", companyInfo.pac.url, "/f"], { encoding: "utf8" });
  pacSet = set.status === 0;
  if (!pacSet) throw new Error(`the PAC address could not be set: ${set.stdout} ${set.stderr}`);
  note(`company network: proxy ${companyInfo.interceptor.proxy_url} (NTLM, inspects TLS), PAC ${companyInfo.pac.url}, root in the machine's store`);
}
function stopCompanyNetwork() {
  if (pacSet) spawnSync("reg", ["delete", INTERNET_SETTINGS, "/v", "AutoConfigURL", "/f"], { stdio: "ignore" });
  if (!companyLab) return;
  writeFileSync(companyStopFile, "stop");
  if (companyRootStored) spawnSync("certutil", ["-delstore", "Root", "Puddle lab interceptor root"], { stdio: "ignore" });
}
function companyLog() {
  try {
    return readFileSync(companyInfo.interceptor.log, "utf8");
  } catch (err) {
    return `no company proxy log: ${err.message}`;
  }
}

// ---- the installed app ------------------------------------------------------------------------------

await startForge().catch((err) => {
  console.error(`FAIL the lab forge did not start: ${err}`);
  process.exit(1);
});

if (company) {
  await startCompanyNetwork().catch((err) => {
    console.error(`FAIL the company network did not start: ${err}\n${companyLabLog}`);
    stopCompanyNetwork();
    stopForge();
    process.exit(1);
  });
}

// A release build has no DevTools port; the WebView2 policy for additional browser arguments gives the
// installed app one, which the Terminal tab's rendering check reads (terminal-render.mjs). The value is
// removed at the end.
const DEBUG_PORT = 9223;
try {
  debugPortPolicy(DEBUG_PORT);
} catch (err) {
  console.log(`     no DevTools policy: ${err.message}`);
}

const connectionFile = join(dataDir, "api.json");
const appExe = join(installDir, "puddle-app.exe");
let app;
let appLog = "";
let appExited = false;
/** Starts the installed app, as the Start menu does; its log joins `appLog`. */
function launchApp() {
  const child = spawn(appExe, [], { stdio: ["ignore", "inherit", "pipe"] });
  child.stderr.on("data", (chunk) => {
    appLog += chunk;
    process.stderr.write(chunk);
  });
  onChildEnd(child, (code) => {
    if (child === app) appExited = true;
    console.log(`puddle-app ${child.pid} exited (${code})`);
  });
  app = child;
  appExited = false;
}
launchApp();

let conn;
let uia;

/** Reads the connection file the app writes at start until its API answers; `exited` says when to stop waiting. */
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
const expectedHost = (host) => host === forge || /(^|\.)debian\.org$/.test(host) || /(^|\.)fastly(dns)?\.net$/.test(host);

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

/** What the first-run pages show and press, the same list the page tests and ui/e2e/first-run.spec.ts read. */
const WALK = JSON.parse(readFileSync(join(here, "..", "..", "..", "ui", "src", "lib", "welcome", "walk.json"), "utf8")).steps;
const control = (c) => inMain(c.role, c.name);
/** Controls on other pages the walk presses, and the closed details each sits behind. */
const TERMINAL_OPTIONS = JSON.parse(readFileSync(join(here, "..", "..", "..", "ui", "src", "lib", "welcome", "walk.json"), "utf8")).behindDetails.terminal;

/** The first-run flow, step by step, ending on the Workspaces page that Open Puddle leads to. */
async function firstRunToCreateForm() {
  for (const [i, step] of WALK.entries()) {
    await uia.waitFor(`the "${step.heading}" heading`, heading(step.heading), i === 0 ? 120_000 : undefined);
    if (step.start) {
      // The check waits for the person to press Start (Continue stays disabled until the run is done).
      await uia.click(step.start.name, control(step.start));
      // 6. Its report, copied to the Windows clipboard.
      await copyTheReport();
    }
    if (step.id === "interception") {
      const mode = await uia.waitFor("the recommended interception mode", { window: MAIN, role: "radio", nameRegex: "except a known list" });
      check("the recommended interception mode is preselected", mode.checked === true, JSON.stringify(mode));
    }
    if (step.id === "connect") {
      const code = await uia.waitFor("the bundled code-server choice", { window: MAIN, role: "radio", nameRegex: "Built-in VS Code server" });
      check("code-server is preselected on the Connect step", code.checked === true, JSON.stringify(code));
    }
    if (step.continue) await uia.click(`${step.continue.name} (from ${step.heading})`, control(step.continue));
  }
}

/** Fills the New workspace form as a person does and presses Create. */
async function createFromTheForm() {
  // The empty list offers a one-line form; More options opens the full New workspace dialog.
  await uia.click("More options", inMain("button", "More options"));
  await uia.waitFor("the New workspace dialog", { window: MAIN, localized: "dialog", name: "New workspace" });
  await uia.fill("the repository address", { window: MAIN, role: "edit", nameRegex: "^Git repository \\(HTTPS" }, httpsUrl);
  await uia.fill("the name", { window: MAIN, role: "edit", name: "Name" }, WORKSPACE);
  // Enter in the Name field submits the dialog's form: two buttons are called "Create workspace" (the Workspaces
  // page's own one-line form sits behind the dialog), and a click by name may reach that one.
  await uia.type("the Name field", { window: MAIN, role: "edit", name: "Name" }, "", { enter: true });
}

/**
 * Company mode: the traffic of the run went through the company proxy, by the app's own report and by
 * the proxy's own log (the report says what the app decided, the log what the proxy saw).
 */
async function checkTheCompanyProxyCarriedTheTraffic(branch) {
  const health = (await api("GET", "/api/network-health")).json;
  const port = String(companyInfo.interceptor.port);
  note(`network health: ${JSON.stringify({ proxy: health?.proxy, sign_in: health?.sign_in, pull_proxy: health?.pull_proxy })}`.slice(0, 1500));
  check("the app found the PAC file the machine is set to", health?.proxy?.pac_url === companyInfo.pac.url, `${health?.proxy?.pac_url} (wanted ${companyInfo.pac.url})`);
  const attempts = health?.sign_in?.attempts ?? [];
  check("the app signed in to the company proxy with the Windows sign-in", attempts.some((a) => a.result === "signed_in" && /ntlm|negotiate/i.test(a.scheme ?? "")), JSON.stringify(attempts).slice(0, 500));
  const roots = health?.roots?.certificates ?? [];
  check("the company root is among the roots Puddle copies into workspaces", roots.some((r) => /Puddle lab interceptor root/.test(r.subject ?? "")), `${roots.length} certificates`);
  const route = (health?.routes ?? []).find((r) => r.host === forge);
  check("the route chosen for the forge is the company proxy, from the PAC", route !== undefined && route.source === "pac" && route.hops.some((h) => h.includes(port)), JSON.stringify(route ?? health?.routes ?? []).slice(0, 500));
  check("image pulls go through the company proxy too", health?.pull_proxy?.via_upstream === true, JSON.stringify(health?.pull_proxy));

  const log = companyLog();
  const signIns = log.split(/\r?\n/).filter((l) => /NTLM leg3 .* accepted user=/.test(l));
  check("the company proxy's log holds the NTLM sign-in", signIns.length > 0, `${signIns.length} sign-ins`);
  const flows = log.split(/\r?\n/).filter((l) => l.includes("] FLOW ") && l.includes(`https://${forge}/`));
  check("the company proxy decrypted the clone's requests to the forge", flows.some((l) => /git-upload-pack/.test(l) && / 200$/.test(l)), flows.slice(0, 3).join(" | "));
  check("the company proxy decrypted the push to the forge", flows.some((l) => /git-receive-pack/.test(l) && / 200$/.test(l)), flows.filter((l) => /receive-pack/.test(l)).join(" | ") || `${branch}: no receive-pack line, ${flows.length} forge lines`);
  const archive = log.split(/\r?\n/).filter((l) => l.includes("] FLOW GET http://deb.debian.org/") && / 200$/.test(l));
  check("the company proxy carried the package archive's requests (plain HTTP, signed in)", archive.length > 0, `${archive.length} answered requests`);
}

// ---- the Windows shell around the window (desktop.ps1) ---------------------------------------------

const desktopScript = join(here, "desktop.ps1");
/** The Start menu shortcut's AppUserModelID, which the app's notifications carry. */
const appUserModelId = JSON.parse(readFileSync(join(here, "..", "tauri.conf.json"), "utf8")).identifier;
/** The main window's title: "Puddle", with the waiting count when the badge shows one (a page's name before it, in older builds); not the editor's. */
const MAIN_TITLE = `^(?!${WORKSPACE} - )(.+ - )?Puddle( \\(.+\\))?$`;

/** One desktop.ps1 action; its output, trimmed. */
function desktop(action, ...args) {
  const run = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", desktopScript, action, ...args], { encoding: "utf8", timeout: 120_000 });
  if (run.status !== 0) throw new Error(`desktop.ps1 ${action} failed (${run.status}): ${(run.stderr || run.stdout || String(run.error)).slice(0, 400)}`);
  return run.stdout.trim();
}
/** The app's notifications in the Action Center, oldest first: [{ title, body }]. */
const toasts = () => [].concat(JSON.parse(desktop("toasts", appUserModelId) || "[]"));
/** The main window: `state`, or press `front`, `minimize` or `close`; returns { found, visible, minimized, foreground }. */
const mainWindow = (op = "state") => JSON.parse(desktop("window", String(app.pid), MAIN_TITLE, op));
/** Empties the app's notifications in the Action Center, so a phase counts only its own. */
const clearToasts = () => desktop("toasts-clear", appUserModelId);
/** The notifications in `now` that `before` did not have (the Action Center keeps a bounded, unordered list). */
function newSince(before, now) {
  const seen = before.map((t) => `${t.title}\n${t.body}`);
  return now.filter((t) => {
    const at = seen.indexOf(`${t.title}\n${t.body}`);
    if (at === -1) return true;
    seen.splice(at, 1);
    return false;
  });
}
const describeToasts = (list) => list.map((t) => `"${t.title}" / "${t.body}"`).join("; ") || "none";

/** A regular expression that matches `text` literally (JavaScript and .NET read it the same). */
const literally = (text) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/** The first run's System check: "Copy report" puts the report, as JSON, on the Windows clipboard. */
async function copyTheReport() {
  const placeholder = `not the report ${Date.now()}`;
  try {
    desktop("clipboard-set", placeholder);
    mainWindow("front");
    const copy = WALK.find((step) => step.afterStart).afterStart;
    await uia.click(copy.name, control(copy), 120_000);
    const said = await uia
      .waitFor("what the page says about the copy", { window: MAIN, nameRegex: "^(Report copied\\.|The browser didn't let Puddle copy)" }, 20_000)
      .catch(() => undefined);
    const text = JSON.parse(desktop("clipboard-get"));
    let report;
    try {
      report = JSON.parse(text);
    } catch {
      report = undefined;
    }
    check(
      "Copy report puts the system check's report, as JSON, on the Windows clipboard",
      report?.schema_version !== undefined && Array.isArray(report.checks) && report.checks.length > 0,
      `the page said "${said?.name ?? "nothing"}"; the clipboard holds ${text === placeholder ? "what it held before" : JSON.stringify(text.slice(0, 160))}`,
    );
  } catch (err) {
    check("Copy report puts the system check's report, as JSON, on the Windows clipboard", false, err.message);
  }
}

/** The names of the terminal's lines in the window (screen reader mode lists each line). */
async function terminalLines() {
  return (await uia.find({ window: MAIN, role: "listitem" }).catch(() => [])).map((e) => e.name);
}

/** The workspace's Terminal tab: a shell as the session user in the start folder, with sudo, and no error page. */
async function terminalTab(startFolder) {
  const whatShows = await step("the workspace's Terminal tab opens", async () => {
    mainWindow("front");
    await uia.click("the Workspaces link", { window: MAIN, role: "link", nameRegex: "^Workspaces" });
    await uia.click(`the ${WORKSPACE} workspace`, inMain("link", WORKSPACE));
    await uia.click("the Terminal tab", inMain("link", "Terminal"));
    return uia.waitFor("the Terminal page or an error page", { window: MAIN, localized: "heading", nameRegex: "^(Terminal|Something went wrong)$" }, 60_000);
  });
  // The route's code runs as the page renders: its New terminal button, or the error page instead.
  await uia.waitFor("the New terminal button or an error page", { window: MAIN, nameRegex: "^(New terminal|Something went wrong)$" }, 60_000).catch(() => undefined);
  const errorPage = await uia.find({ window: MAIN, localized: "heading", name: "Something went wrong" });
  const texts = errorPage.length > 0 ? (await uia.dump(MAIN).catch(() => [])).filter((l) => /\| /.test(l)).slice(0, 40).join(" ¦ ") : "";
  check("the Terminal tab shows the terminal, no error page", whatShows.name === "Terminal" && errorPage.length === 0, errorPage.length > 0 ? `the error page: ${texts}` : whatShows.name);
  if (errorPage.length > 0) {
    screenshot("terminal-error-page");
    check("the terminal reads agent, the start folder and sudo-ok", false, "no terminal: the tab shows an error page");
    return;
  }
  await step("a new terminal opens, its lines listed for a screen reader", async () => {
    // The switch sits in a closed details, which is not in the UI Automation tree: open its summary first.
    const modeSwitch = { window: MAIN, ...TERMINAL_OPTIONS.control };
    if ((await uia.find(modeSwitch)).length === 0) await uia.click(TERMINAL_OPTIONS.summary.name, { window: MAIN, ...TERMINAL_OPTIONS.summary });
    const mode = await uia.waitFor("the Screen reader mode switch", modeSwitch);
    if (mode.checked !== true) await uia.click("Screen reader mode", modeSwitch);
    await until("Screen reader mode to be on", async () => (await uia.find(modeSwitch))[0]?.checked === true, 10_000, 500);
    // The page opens a first terminal itself when there is none; New terminal only when it did not.
    const opened = await uia.waitFor("the first terminal tab", { window: MAIN, role: "tab", nameRegex: "^Terminal \\d+$" }, 15_000).catch(() => undefined);
    if (!opened) await uia.click("New terminal", inMain("button", "New terminal"));
    await uia.waitFor("the shell's prompt", { window: MAIN, role: "listitem", nameRegex: "[$#]\\s*$" }, 120_000);
  });
  const command = "whoami; pwd; sudo -n true && echo sudo-ok";
  await step("the command is typed into the terminal", () => {
    // The keys go to the window in front: make sure that is this one.
    if (!mainWindow("front").foreground) throw new Error("the main window did not come to the front");
    return uia.type("the terminal's input", { window: MAIN, role: "edit", nameRegex: `^Terminal \\d+, ${literally(WORKSPACE)}$` }, command, { enter: true });
  });
  const shows = (text) =>
    uia.waitFor(`the terminal line "${text}"`, { window: MAIN, role: "listitem", nameRegex: `^\\s*${literally(text)}\\s*$` }, 60_000).then(
      () => true,
      () => false,
    );
  const [user, folder, sudo] = [await shows("agent"), await shows(startFolder), await shows("sudo-ok")];
  screenshot("terminal");
  const lines = (await terminalLines()).filter((l) => l.trim()).slice(-12).join(" ¦ ");
  check("the terminal reads agent, the start folder and sudo-ok", user && folder && sudo, `whoami ${user}, pwd ${startFolder} ${folder}, sudo ${sudo}; lines: ${lines}`);
  await terminalRendering();
}

/**
 * The close button on a page that failed to load: the window's page is made to fail on purpose (every
 * request refused, then a reload shows the browser's error page), and the close button still hides the
 * window in the tray, the app keeps running, and the window comes back from a second start.
 */
async function closeOnABrokenPage() {
  mainWindow("front");
  const broken = await breakThePage(DEBUG_PORT);
  // The control: Puddle's own navigation is gone from the window, so the close below is pressed on a page
  // that is not Puddle's.
  const gone = await until("Puddle's navigation to be gone", async () => (await uia.find(inMain("link", "Workspaces"))).length === 0, 30_000, 1000).then(() => true, () => false);
  check("the main window's page failed to load on purpose (the control)", gone && broken.refused.length > 0, `refused: ${broken.refused.join(", ")}; navigation gone: ${gone}`);
  try {
    const hidden = [];
    for (const round of [1, 2]) {
      mainWindow("front");
      mainWindow("close");
      const s = await windowBecomes(`the window to hide on the broken page (close ${round})`, (w) => !w.visible).catch(() => mainWindow());
      hidden.push(!s.visible && !appExited);
      await reopen().catch(() => {});
    }
    check("the close button hides the window in the tray on a page that failed to load", hidden.every(Boolean), `hidden after each close: ${hidden.join(", ")}; exited: ${appExited}`);
  } finally {
    await broken.restore();
  }
}

/** Colours, font, size and a mouse selection of the terminal, read from the page over the policy's DevTools port. */
async function terminalRendering() {
  // Two SGR sequences by printf; the words appear in the output only (the typed command holds the escapes, not the words).
  const command = "printf '\\033[31mCOLOR-RED\\033[0m \\033[1;32mCOLOR-GREEN\\033[0m\\n'";
  await step("the colour sample is printed", async () => {
    await uia.type("the terminal's input", { window: MAIN, role: "edit", name: `Terminal 1, ${WORKSPACE}` }, command, { enter: true });
    await uia.waitFor("the colour sample line", { window: MAIN, role: "listitem", nameRegex: "COLOR-RED COLOR-GREEN" }, 60_000);
  });
  let report;
  try {
    report = await inspectTerminal(DEBUG_PORT);
  } catch (err) {
    check("the terminal's rendering could be read over DevTools", false, String(err?.message ?? err));
    return;
  }
  for (const c of report.checks) check(`terminal rendering: ${c.name}`, c.ok, c.detail);
  writeFileSync(join(out, "terminal-render.json"), JSON.stringify(report.dump, null, 2));
  if (report.checks.some((c) => !c.ok)) screenshot("terminal-rendering");
}

/** The guest's clock, in seconds. */
async function guestNow(id) {
  const r = await guest(id, "date +%s.%N", { root: true, within: 60_000 });
  return Number.parseFloat(r.text.trim().split("\n").pop());
}

/** Waits until the main window is shown or hidden; its state. */
const windowBecomes = (what, wanted) => until(what, () => {
  const s = mainWindow();
  return s.found && wanted(s) ? s : false;
}, 30_000, 500);

/** A second start of the program, as from the Start menu: it brings the running app's window back. */
async function reopen() {
  spawnSync(appExe, [], { stdio: "ignore", timeout: 60_000 });
  return windowBecomes("the window to come back", (s) => s.visible && !s.minimized);
}

/** The close button hides the window in the tray, tells so once, and the workspace runs on meanwhile. */
async function closeToTray(id) {
  const NOTICE = "Puddle is still running";
  clearToasts();
  const before = toasts();
  const beat = await guest(id, "rm -f /tmp/heartbeat; setsid sh -c 'while :; do date +%s.%N >> /tmp/heartbeat; sleep 0.5; done' </dev/null >/dev/null 2>&1 & echo $! > /tmp/heartbeat.pid; sleep 2; wc -l < /tmp/heartbeat", { root: true, within: 60_000 });
  note(`the guest's heartbeat started: ${beat.text.trim()} beats so far`);
  const from = await guestNow(id);
  mainWindow("front");
  const hidden = [];
  for (const round of [1, 2]) {
    mainWindow("close");
    const s = await windowBecomes(`the window to hide (close ${round})`, (w) => !w.visible).catch(() => mainWindow());
    hidden.push(!s.visible && !appExited);
    // Hidden long enough for a build to notice: half a minute the first time.
    await sleep(round === 1 ? 30_000 : 10_000);
    await reopen().catch(() => {});
  }
  const to = await guestNow(id);
  check("the close button hides the window in the tray and Puddle keeps running", hidden.every(Boolean), `hidden after each close: ${hidden.join(", ")}; exited: ${appExited}`);
  const told = newSince(before, toasts()).filter((t) => t.title === NOTICE);
  check("closing twice makes exactly one 'Puddle is still running' Windows notification", told.length === 1, `${told.length}: ${describeToasts(told)}`);
  const beats = (await guest(id, "kill $(cat /tmp/heartbeat.pid); cat /tmp/heartbeat", { root: true, within: 60_000 })).text
    .split("\n")
    .map(Number.parseFloat)
    .filter((t) => Number.isFinite(t) && t >= from - 1 && t <= to + 1);
  const marks = [from, ...beats, to].sort((a, b) => a - b);
  const gap = Math.max(...marks.slice(1).map((t, i) => t - marks[i]));
  check(
    "the guest's heartbeat has no gap over 2 s while the window is hidden",
    Number.isFinite(from) && Number.isFinite(to) && beats.length > 0 && gap <= 2,
    `${beats.length} beats over ${(to - from).toFixed(1)} s, longest gap ${gap.toFixed(2)} s`,
  );
}

/** A guest request to a host nobody allowed; waits until the inbox lists it. */
async function newRequest(id, host) {
  await guest(id, `curl -s -o /dev/null --max-time 20 https://${host}/ || git ls-remote https://${host}/ >/dev/null 2>&1 || true`, { within: 90_000 });
  return until(`the request for ${host} in the inbox`, async () => (await api("GET", "/api/pending")).json?.requests?.find((r) => r.host === host && r.state === "requested"), 60_000, 1000).catch(() => undefined);
}

/** A new request notifies only while the window is not in front, once, naming the workspace and the host. */
async function requestNotifications(id) {
  const front = mainWindow("front");
  clearToasts();
  const before = toasts();
  const inFront = await newRequest(id, "example.com");
  // Longer than the ten seconds a group of requests is held, so a late summary shows too.
  await sleep(15_000);
  const stillFront = mainWindow();
  const whileFront = newSince(before, toasts());
  check(
    "a new request makes no Windows notification while the window is in front",
    inFront !== undefined && front.foreground && stillFront.foreground && whileFront.length === 0,
    `request ${inFront ? "listed" : "not listed"}; in front before ${front.foreground}, after ${stillFront.foreground}; new: ${describeToasts(whileFront)}`,
  );
  const minimized = mainWindow("minimize");
  const middle = toasts();
  const away = await newRequest(id, "example.org");
  const first = await until("a notification for the request", () => newSince(middle, toasts()).length > 0, 30_000, 1000).catch(() => false);
  await sleep(15_000);
  const whileAway = newSince(middle, toasts());
  const names = whileAway.length === 1 && whileAway[0].title.startsWith(`${WORKSPACE} `) && /^example\.org:\d+ /.test(whileAway[0].body);
  check(
    "a new request while the window is minimized makes exactly one Windows notification, naming the workspace and the host",
    away !== undefined && minimized.minimized && first && whileAway.length === 1 && names,
    `request ${away ? "listed" : "not listed"}; minimized ${minimized.minimized}; new: ${describeToasts(whileAway)}`,
  );
  mainWindow("front");
}

/** Activity's "Export JSON lines" saves the records as a .jsonl file in Downloads. */
async function exportActivity() {
  const downloads = join(homedir(), "Downloads");
  const earlier = new Set(existsSync(downloads) ? readdirSync(downloads) : []);
  const saved = await step("Activity's Export JSON lines is pressed", async () => {
    mainWindow("front");
    await uia.click("the Activity link", { window: MAIN, role: "link", nameRegex: "^Activity" });
    await uia.waitFor("the Activity heading", heading("Activity"));
    await uia.call({ action: "focus", ...inMain("button", "Export JSON lines") });
    await uia.click("Export JSON lines", inMain("button", "Export JSON lines"));
    return uia.waitFor("the page's word on the export", { window: MAIN, nameRegex: "^(Saved [\\d,.\\s]+ records? as puddle-activity-.+\\.jsonl\\.|Nothing to export|Export cancelled)" }, 120_000);
  }).catch((err) => ({ name: `nothing: ${err.message}` }));
  const said = /^Saved ([\d,.\s]+) records? as (puddle-activity-.+\.jsonl)\.$/.exec(saved.name ?? "");
  const file = await until(
    "the export in Downloads",
    () => readdirSync(downloads).find((f) => !earlier.has(f) && /^puddle-activity-.*\.jsonl$/.test(f)),
    60_000,
    1000,
  ).catch(() => undefined);
  const lines = file ? readFileSync(join(downloads, file), "utf8").split("\n").filter((l) => l.trim()) : [];
  const valid = lines.every((l) => {
    try {
      const record = JSON.parse(l);
      return record !== null && typeof record === "object" && !Array.isArray(record);
    } catch {
      return false;
    }
  });
  const count = said ? Number(said[1].replace(/\D/g, "")) : undefined;
  check(
    "Export JSON lines saves the records in Downloads, one JSON object a line, as many as the page says",
    file !== undefined && lines.length > 0 && valid && said?.[2] === file && count === lines.length,
    `page: "${saved.name}"; file: ${file ?? "none"} with ${lines.length} lines, all JSON ${valid}; new in Downloads: ${readdirSync(downloads).filter((f) => !earlier.has(f)).join(", ") || "nothing"}`,
  );
  check("the export holds no token", lines.length > 0 && !lines.some((l) => l.includes(info.token)), `${lines.length} lines`);
}

/** The tray's "Quit Puddle" with the workspace running ends every process of Puddle, and the next start lists the workspace as stopped. */
async function quitFromTheTray(id) {
  const started = Date.now();
  const chosen = desktop("tray-quit");
  const left = await until(
    "no Puddle process left",
    () => {
      const now = JSON.parse(desktop("processes", "puddle-app", "puddle", "msb"));
      return now.length === 0 ? [] : false;
    },
    90_000,
    2000,
  ).catch(() => JSON.parse(desktop("processes", "puddle-app", "puddle", "msb")));
  check(
    "the tray's Quit Puddle with the workspace running leaves no puddle-app.exe, puddle.exe or msb.exe process within 90 s",
    chosen === "quit chosen" && left.length === 0,
    `${chosen}; after ${Math.round((Date.now() - started) / 1000)} s: ${left.map((p) => `${p.name} ${p.id}`).join(", ") || "none"}`,
  );
  // Whatever is left would hold the data folder: end it before the next start.
  for (const p of left) spawnSync("taskkill", ["/PID", String(p.id), "/T", "/F"], { stdio: "ignore" });
  uia?.close();
  uia = undefined;
  launchApp();
  await connectToApp();
  const w = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.id === id);
  check("a new start lists the workspace as stopped", w?.status === "stopped", JSON.stringify(w ?? null).slice(0, 300));
}

// ---- the run ---------------------------------------------------------------------------------------

try {
  await connectToApp();
  check("the installed app starts and answers its API", true, conn.url);
  uia = await Uia.start(app.pid);
  // Windows asks once to trust the app's own localhost certificate (the editor's origin), whenever the
  // app first needs it; the user answers Yes, so a poll does, for the whole run.
  const certificate = setInterval(() => {
    uia?.call({ action: "click", window: "^Security Warning$", role: "pane", name: "Yes" }).then((r) => {
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

  // 1. An SSH remote of a host Puddle does not end SSH for is refused with the explicit message (use the HTTPS URL), and nothing is made.
  const ssh = await api("POST", "/api/workspaces", { name: "ssh-refused", repositories: [{ url: sshUrl }] });
  check("an SSH remote is refused with the explicit message", ssh.status >= 400 && ssh.status < 500 && /for another host, use the repository.s HTTPS U/.test(ssh.body), `${ssh.status} ${ssh.body.slice(0, 200)}`);
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
      if (!w) throw new GiveUp("the workspace vanished");
      if (["crashed", "volume_missing"].includes(w.status)) throw new GiveUp(`the workspace is ${w.status}`);
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
    await uia.click("Open isolated VS Code", inMain("button", "Open isolated VS Code"));
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

  if (company) await checkTheCompanyProxyCarriedTheTraffic(branch);

  // 7 to 11. The app's shell around its window, with the workspace running. The direct run only: the
  // company run is about the network, and the shell does not change behind a proxy.
  if (!company) {
    const listed = (await api("GET", "/api/workspaces")).json?.workspaces?.find((w) => w.id === id);
    const phase = (what, run) => run().catch((err) => check(`${what} ran to the end`, false, String(err?.message ?? err)));
    await phase("the Terminal tab", () => terminalTab(listed?.start_folder ?? `/workspaces/${id}`));
    await phase("the close button", () => closeToTray(id));
    await phase("the request notifications", () => requestNotifications(id));
    await phase("the Activity export", exportActivity);
    await phase("the close button on a broken page", closeOnABrokenPage);
    await phase("the tray's Quit", () => quitFromTheTray(id));
  }

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
  debugPortPolicy(undefined);
  writeFileSync(join(out, "app.log"), appLog);
  if (info) writeFileSync(join(out, "forgejo-server.log"), forgeLog());
  if (companyInfo) writeFileSync(join(out, "company-proxy.log"), companyLog());
  if (companyLab) writeFileSync(join(out, "company-lab.out"), companyLabLog);
  uia?.close();
  stopCompanyNetwork();
  stopForge();
  if (!appExited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\nthe usable run passed");
process.exit(0);
