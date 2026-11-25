// SPDX-License-Identifier: AGPL-3.0-or-later
// Microsoft's real VS Code server in the app's real WebView2 window, with no person (Windows only): the
// debug app runs the Puddle host on real microVMs, the server is Microsoft's pinned build (ci/vscode-server.sha256)
// chosen after the consent setting, and the editor window is opened from the Connect step and read over WebView2's
// DevTools Protocol (the debug app opens the main window's port, the editor window's is the next one).
//
//   1. a two-checkout workspace opens in the window with one explorer root per checkout (and the volume);
//   2. a test extension (fixtures/canary-secret) keeps a made-up session in the editor's secret storage;
//   3. the window is closed the way the close button does it and opened again from the Connect step: the extension
//      finds the same session in secret storage (still signed in), on a new extension host.
//   4. two more workspaces, so three editor windows are open at once: each shows its own roots, and a command typed in its
//      terminal runs in its own guest (`hostname`, and the workspace's name); closing all three leaves no editor WebView2 process
//      of those workspaces and no extension host in any guest.
//
// Why the pinned build is put on the volume by this script: the product installs the newest build by itself, and
// this run must not change when Microsoft ships; the script waits for that install, then replaces the build the
// editor starts with the pinned one and restarts the workspace. Automatic updates are switched off for the run.
//
// Environment: PUDDLE_APP_EXE (a debug build without the `fixture` feature, with `runtime/` beside it),
// PUDDLE_AGENT_BIN (the guest agent), MS_EDITOR_PIN (ci/vscode-server.sha256), MS_EDITOR_OUT (a folder for pictures and
// logs), CDP_PORT (default 9340; the editor windows take every second port after it, one per workspace). Plain Node 24, no packages.
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";
import { connect } from "./cdp.mjs";
import { onChildEnd } from "./child-end.mjs";
import { GiveUp, until } from "./poll.mjs";
import { Uia } from "./uia.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
if (!process.env.PUDDLE_AGENT_BIN) throw new Error("set PUDDLE_AGENT_BIN to the guest agent");
if (!process.env.MS_EDITOR_PIN) throw new Error("set MS_EDITOR_PIN to ci/vscode-server.sha256");
const out = process.env.MS_EDITOR_OUT ?? join(tmpdir(), "puddle-ms-editor");
mkdirSync(out, { recursive: true });
const mainPort = Number(process.env.CDP_PORT ?? 9340);
const editorPort = mainPort + 1;
const NAME = "msedit";
const MICROSOFT_TERMS = "https://code.visualstudio.com/license/server";

// The pin: `<release> <commit> <sha256>`.
const [release, commit, sha256] = readFileSync(process.env.MS_EDITOR_PIN, "utf8")
  .split("\n")
  .find((line) => line.trim() && !line.startsWith("#"))
  .trim()
  .split(/\s+/);

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}
const note = (text) => console.log(`     ${text}`);

// ---- the app ----------------------------------------------------------------------------------------

const dataDir = mkdtempSync(join(tmpdir(), "puddle-ms-data-"));
const connectionFile = join(dataDir, "api.json");
const app = spawn(exe, [], {
  stdio: ["ignore", "inherit", "pipe"],
  env: {
    ...process.env,
    PUDDLE_APP_DATA_DIR: dataDir,
    PUDDLE_CONNECTION_FILE: connectionFile,
    PUDDLE_APP_DEBUG_PORT: String(mainPort),
    PUDDLE_LOG: "puddle_app=debug,puddle_host=debug,info",
  },
});
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

// ---- the guest, through the runtime's own msb -----------------------------------------------------

const runtimeDir = join(dirname(exe), "runtime");
const msbEnv = {
  ...process.env,
  MSB_PATH: join(runtimeDir, "msb.exe"),
  MSB_HOME: join(dataDir, "msb"),
  MSB_CONFIG_PATH: join(dataDir, "msb", "config.json"),
};

/** A script in the guest as root; the script text is one argument, so nothing of it runs on this machine. */
function guest(id, script, within = 600_000) {
  const child = spawn(msbEnv.MSB_PATH, ["exec", id, "--", "sh", "-c", 'exec sh -c "$1"', "_", script], { env: msbEnv, stdio: ["ignore", "pipe", "pipe"] });
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

// ---- windows over the DevTools Protocol ---------------------------------------------------------

/** The first page target of the WebView2 on `port` whose url matches `pattern`. */
async function pageTarget(port, pattern) {
  const list = await (await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(5000) })).json();
  return list.find((t) => t.type === "page" && pattern.test(t.url));
}
async function page(port, pattern, within = 120_000) {
  const target = await until(`a page matching ${pattern} on DevTools port ${port}`, () => pageTarget(port, pattern), within, 500);
  return connect(target.webSocketDebuggerUrl);
}
let shots = 0;
async function picture(handle, name) {
  try {
    const shot = await handle.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${String(++shots).padStart(2, "0")}-${name}.png`), Buffer.from(shot.data, "base64"));
  } catch (err) {
    note(`no picture of ${name}: ${err.message}`);
  }
}

const EXPLORER_READY = "document.querySelectorAll('.explorer-folders-view .monaco-list-row .label-name').length >= 2";
const EXPLORER_ROWS = `[...document.querySelectorAll('.explorer-folders-view .monaco-list-row')].map(r => ({ name: r.querySelector('.label-name')?.textContent, level: r.getAttribute('aria-level') }))`;
const WINDOW_STATE = `(() => {
  const text = (e, n) => (e?.textContent || '').replace(/\\s+/g, ' ').slice(0, n);
  const all = (selector, n) => [...document.querySelectorAll(selector)].map(e => text(e, n));
  return { href: location.href, rows: all('.explorer-folders-view .monaco-list-row', 80), sidebar: text(document.querySelector('.part.sidebar .title-label'), 80),
    dialogs: all('.monaco-dialog-box', 400), notifications: all('.notifications-toasts .notification-list-item', 300), status: text(document.querySelector('.part.statusbar'), 400),
    body: text(document.body, 300) };
})()`;

/** Waits for the editor window's explorer and returns where the window is and its rows; a failure says what it shows. */
async function explorerOf(handle, what) {
  const deadline = Date.now() + 300_000;
  for (;;) {
    const ready = await handle.evaluate(EXPLORER_READY).catch(() => false);
    if (ready) break;
    if (Date.now() > deadline) {
      const state = await handle.evaluate(WINDOW_STATE).catch((e) => `(not read: ${e.message})`);
      await picture(handle, `${what}-no-explorer`);
      throw new Error(`${what}: the explorer did not get ready; the window shows ${JSON.stringify(state)}`);
    }
    await sleep(1000);
  }
  return {
    rows: await handle.evaluate(EXPLORER_ROWS),
    where: await handle.evaluate("({ href: location.href, search: decodeURIComponent(location.search), path: location.pathname })"),
  };
}

// ---- the run ---------------------------------------------------------------------------------------

let uia;
let id;

/** Presses Connect, then "Open isolated VS Code", in the main window as a person does. */
async function connectFromTheMainWindow(name = NAME) {
  const main = await page(mainPort, /^http:\/\/127\.0\.0\.1:\d+\//);
  try {
    const clicked = (script) => main.evaluate(`(() => { ${script} })()`, true);
    await clicked(`document.querySelector('nav[aria-label="Main"] a[href="/workspaces"]')?.click(); return true;`);
    await until(
      "the Connect button of the workspace",
      () => main.evaluate(`[...document.querySelectorAll('button')].some((b) => b.textContent.replace(/\\s+/g, ' ').trim() === 'Connect to ${name}')`),
      60_000,
      500,
    );
    await clicked(`[...document.querySelectorAll('button')].find((b) => b.textContent.replace(/\\s+/g, ' ').trim() === 'Connect to ${name}').click(); return true;`);
    await until(
      "the Connect step's browser button",
      () => main.evaluate(`[...document.querySelectorAll('button')].some((b) => b.textContent.trim() === 'Open isolated VS Code')`),
      30_000,
      300,
    );
    await clicked(`[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Open isolated VS Code').click(); return true;`);
  } finally {
    main.close();
  }
}

const canaryLog = async () => (await guest(id, "cat /tmp/canary-secret.log 2>/dev/null; true", 60_000)).text;
const serverNow = () => guest(id, "curl -s --max-time 3 http://127.0.0.1:41337/version", 60_000).then((r) => r.text.trim());


/** The workspace `name` made with no repository, run once and not busy; returns its id. */
async function createRunning(name, memory = 1536) {
  const made = await api("POST", "/api/workspaces", { name, repositories: [], memory_mib: memory });
  check(`the workspace ${name} is accepted`, made.status === 202, `${made.status} ${made.body.slice(0, 200)}`);
  let kicked = false;
  const w = await until(
    `the workspace ${name} to run`,
    async () => {
      const found = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.name === name);
      if (!found) return false;
      if (["crashed", "volume_missing"].includes(found.status)) throw new GiveUp(`${name} is ${found.status}`);
      if (found.status === "running" && !found.busy) return found;
      if (!kicked && !found.busy && ["created", "stopped"].includes(found.status)) {
        kicked = true;
        await api("POST", `/api/workspaces/${found.id}/start`);
      }
      return false;
    },
    900_000,
    2000,
  );
  return w.id;
}

/** Waits until the editor's own install of the newest build is confirmed in `wsId`'s guest (as for the first workspace). */
async function waitOwnInstall(wsId) {
  const dir = `/workspace/.puddle/vscode-server/servers`;
  await until(
    `the editor of ${wsId} to install its own build`,
    async () => {
      const named = (await guest(wsId, `cat ${dir}/current 2>/dev/null`, 60_000)).text.trim();
      if (!/^[0-9a-f]{40}$/.test(named)) return false;
      const confirmed = (await guest(wsId, `cat ${dir}/confirmed 2>/dev/null`, 60_000)).text.trim();
      return confirmed === named;
    },
    600_000,
    5000,
  );
  return dir;
}

/** The pinned build and the checkouts `checkouts`, put on the volume of `wsId` as root. */
async function putPinned(wsId, dir, checkouts) {
  const script = [
    "set -eu",
    `builds=${dir}; commit=${commit}; tmp=$builds/$commit.tar.gz.partial`,
    `curl -fsSL --retry 3 --retry-delay 5 -o "$tmp" "https://update.code.visualstudio.com/commit:$commit/server-linux-x64-web/stable"`,
    `echo "${sha256}  $tmp" | sha256sum -c -`,
    `rm -rf "$builds/$commit.partial" "$builds/$commit"`,
    `mkdir "$builds/$commit.partial"`,
    `tar -xzf "$tmp" -C "$builds/$commit.partial" --strip-components=1`,
    `rm -f "$tmp"`,
    `[ -f "$builds/$commit.partial/bin/code-server" ]`,
    `mv "$builds/$commit.partial" "$builds/$commit"`,
    `: > "$builds/$commit/.puddle-installed"`,
    `printf '%s' "$commit" > "$builds/current"`,
    `cd /workspace`,
    ...checkouts.map((c) => `[ -d ${c}/.git ] || git init -q ${c}`),
    `if id agent >/dev/null 2>&1; then chown -R agent:agent ${checkouts.join(" ")}; fi`,
    "echo pinned-installed",
  ].join("\n");
  return guest(wsId, script, 900_000);
}

/** Stops and starts the workspace, waiting for each end. */
async function restart(wsId) {
  for (const step of ["stop", "start"]) {
    await api("POST", `/api/workspaces/${wsId}/${step}`);
    await until(
      `${wsId} to finish ${step}`,
      async () => {
        const w = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.id === wsId);
        return w && !w.busy && w.status === (step === "stop" ? "stopped" : "running") ? w : false;
      },
      300_000,
      2000,
    );
  }
}

/** The editor window of workspace `wsId`: its own WebView2 process has its own DevTools port (every second port after the first window's). */
async function editorOf(wsId, slot) {
  return page(editorPort + 2 * slot, new RegExp(`^https?://${wsId}\\.localhost:\\d+/`), 240_000);
}

/** Opens the editor's integrated terminal and types one command, as a person does (real input over the DevTools Protocol). */
async function typeInTerminal(handle, command) {
  await handle.send("Page.bringToFront");
  const key = async (spec) => {
    await handle.send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...spec });
    if (spec.text) await handle.send("Input.dispatchKeyEvent", { type: "char", ...spec });
    await handle.send("Input.dispatchKeyEvent", { type: "keyUp", ...spec });
  };
  const terminalUp = () => handle.evaluate(`!!document.querySelector('.terminal-wrapper .xterm-helper-textarea, .xterm-helper-textarea')`).catch(() => false);
  // A click on the editor area gives the workbench the keyboard focus.
  const click = async (x, y) => {
    for (const type of ["mousePressed", "mouseReleased"]) await handle.send("Input.dispatchMouseEvent", { type, x, y, button: "left", clickCount: 1 });
  };
  await click(500, 300);
  // Ctrl+Shift+` creates a terminal; when the keys do not reach the workbench, the command palette does the same.
  await key({ key: "`", code: "Backquote", windowsVirtualKeyCode: 192, modifiers: 10 });
  // A multi-root workspace asks first in which folder the terminal starts (a multi-root window shows a folder picker): take the first.
  const asksFolder = () => handle.evaluate(`[...document.querySelectorAll('.quick-input-widget input')].some((i) => i.offsetParent !== null && /working directory/i.test(i.placeholder))`).catch(() => false);
  await until("the terminal or its folder question", async () => (await terminalUp()) || (await asksFolder()), 15_000, 500).catch(() => false);
  if (await asksFolder()) await key({ key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
  let up = await until("the terminal after Ctrl+Shift+`", terminalUp, 15_000, 500).catch(() => false);
  if (!up) {
    await key({ key: "F1", code: "F1", windowsVirtualKeyCode: 112 });
    await sleep(1500);
    await handle.send("Input.insertText", { text: "Terminal: Create New Terminal" });
    await sleep(1500);
    await key({ key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
    up = await until("the terminal after the command palette", terminalUp, 45_000, 500).catch(() => false);
  }
  if (!up) {
    await picture(handle, "no-terminal");
    throw new Error(`no terminal opened; the window shows ${JSON.stringify(await handle.evaluate(WINDOW_STATE).catch(() => null))}`);
  }
  await sleep(3000);
  await handle.send("Input.insertText", { text: command });
  await key({ key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: "\r" });
}

/** The msedgewebview2 processes whose command line names the data folder of any of `ids`. */
function editorWebViewProcesses(ids) {
  const pattern = ids.map((i) => `webview[\\\\/]${i}`).join("|");
  const r = spawnSync(
    "powershell",
    ["-NoProfile", "-Command", `Get-CimInstance Win32_Process -Filter "Name='msedgewebview2.exe'" | Where-Object { $_.CommandLine -match '${pattern}' } | ForEach-Object { $_.ProcessId }`],
    { encoding: "utf8" },
  );
  return r.stdout.split(/\s+/).filter(Boolean);
}

try {
  await until(
    "the app's connection file",
    () => {
      if (appExited) throw new GiveUp("puddle-app exited before it wrote its connection file");
      conn = JSON.parse(readFileSync(connectionFile, "utf8"));
      return conn?.url && conn?.token;
    },
    180_000,
    1000,
  );
  await until("the API", async () => (await api("GET", "/api/health")).status === 200, 60_000);
  check("the debug app starts on the Puddle host and answers its API", true, conn.url);
  uia = await Uia.start(app.pid);
  // Windows asks once to trust the app's own localhost certificate; a person answers Yes, so a poll does.
  const certificate = setInterval(() => {
    uia.call({ action: "click", window: "^Security Warning$", role: "pane", name: "Yes" }).then((r) => {
      if (r.found) note("the Windows certificate dialog for localhost: answered Yes");
    }, () => {});
  }, 2000);
  certificate.unref();

  // The first-run flow has the whole window: skip it as a person does, so the sidebar and the workspace list show.
  const first = await page(mainPort, /^http:\/\/127\.0\.0\.1:\d+\//);
  await until("the Skip setup button", () => first.evaluate(`[...document.querySelectorAll('button')].some((b) => b.textContent.trim() === 'Skip setup')`), 120_000, 500);
  await first.evaluate(`[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Skip setup').click(); true`, true);
  first.close();

  // The consent setting, then the choice of Microsoft's server (the API checks the consent first); updates off for the run.
  const consent = await api("PUT", "/api/consents/vscode_server", { decision: "granted", terms_version: MICROSOFT_TERMS });
  check("the consent to Microsoft's server terms is recorded", consent.status === 200, `${consent.status} ${consent.body.slice(0, 200)}`);
  const chosen = await api("PUT", "/api/settings", { vscode_server: { server: "microsoft", telemetry: false, auto_update: false } });
  check("Microsoft's VS Code server is chosen, automatic updates off", chosen.status === 200, `${chosen.status} ${chosen.body.slice(0, 200)}`);

  // A workspace with no repository; the two checkouts are made in the guest below (as the browser-editor VM test does).
  const created = await api("POST", "/api/workspaces", { name: NAME, repositories: [], memory_mib: 2048 });
  check("the workspace create is accepted", created.status === 202, `${created.status} ${created.body.slice(0, 200)}`);
  let started = false;
  const running = await until(
    "the workspace to run",
    async () => {
      const w = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.name === NAME);
      if (!w) return false;
      id = w.id;
      if (["crashed", "volume_missing"].includes(w.status)) throw new GiveUp(`the workspace is ${w.status}`);
      if (w.status === "running" && !w.busy) return w;
      if (!started && !w.busy && ["created", "stopped"].includes(w.status)) {
        started = true;
        await api("POST", `/api/workspaces/${w.id}/start`);
      }
      return false;
    },
    900_000,
    2000,
  );
  check("the workspace is running", running.status === "running", `${running.status}, image ${running.image}, id ${id}`);

  // The product's own install of the newest build comes first; wait until it is installed, running and confirmed (its cleanup
  // of the builds folder has run then) so it can neither overwrite nor delete the pinned build later.
  const builds = `/workspace/.puddle/vscode-server/servers`;
  const own = await until(
    "the editor's own install of Microsoft's newest build",
    async () => {
      const r = await guest(id, `cat ${builds}/current 2>/dev/null`, 60_000);
      const named = r.text.trim();
      if (!/^[0-9a-f]{40}$/.test(named)) return false;
      // `current` is written when the download ends; the agent then starts that build and, once it listens, writes `confirmed`
      // and prunes the builds folder (every `*.partial` and every build that is not named). Downloading the pinned build
      // before that deleted its tarball under the script (run 44080141935), so wait for the confirmation.
      const confirmed = await guest(id, `cat ${builds}/confirmed 2>/dev/null`, 60_000);
      return confirmed.text.trim() === named ? named : false;
    },
    600_000,
    5000,
  ).catch(async (err) => {
    const tail = await guest(id, "tail -n 40 /run/puddle/agent.log", 60_000);
    throw new Error(`${err.message}; the agent says:\n${tail.text}`);
  });
  note(`the editor installed Microsoft's build ${own} by itself; the run uses the pinned ${release} (${commit})`);

  // Pinned build, the test extension and two checkouts, put on the volume as root; then a restart starts the pinned build.
  const files = ["package.json", "extension.js"].map((name) => [name, readFileSync(join(here, "fixtures", "canary-secret", name)).toString("base64")]);
  const data = `/workspace/.puddle/vscode-server`;
  const extension = `${data}/extensions/puddle-test.canary-secret-0.0.1`;
  const registry = JSON.stringify([
    {
      identifier: { id: "puddle-test.canary-secret" },
      version: "0.0.1",
      location: { $mid: 1, path: extension, scheme: "file" },
      relativeLocation: "puddle-test.canary-secret-0.0.1",
      metadata: {},
    },
  ]);
  const setup = [
    "set -eu",
    `builds=${builds}; commit=${commit}; tmp=$builds/$commit.tar.gz.partial`,
    `curl -fsSL --retry 3 --retry-delay 5 -o "$tmp" "https://update.code.visualstudio.com/commit:$commit/server-linux-x64-web/stable"`,
    `ls -l "$tmp"`,
    `echo "${sha256}  $tmp" | sha256sum -c -`,
    `rm -rf "$builds/$commit.partial" "$builds/$commit"`,
    `mkdir "$builds/$commit.partial"`,
    `tar -xzf "$tmp" -C "$builds/$commit.partial" --strip-components=1`,
    `rm -f "$tmp"`,
    `[ -f "$builds/$commit.partial/bin/code-server" ]`,
    `mv "$builds/$commit.partial" "$builds/$commit"`,
    `: > "$builds/$commit/.puddle-installed"`,
    `printf '%s' "$commit" > "$builds/current"`,
    `mkdir -p ${extension}`,
    ...files.map(([name, b64]) => `printf '%s' '${b64}' | base64 -d > ${extension}/${name}`),
    // The server lists the extensions of its default profile in extensions.json; a folder alone is not read as installed.
    `printf '%s' '${registry}' > ${data}/extensions/extensions.json`,
    `cd /workspace`,
    "[ -d api/.git ] || git init -q api",
    "[ -d web/.git ] || git init -q web",
    `if id agent >/dev/null 2>&1; then chown -R agent:agent ${data}/extensions api web; fi`,
    "rm -f /tmp/canary-secret.log",
    "echo pinned-installed",
  ].join("\n");
  const installed = await guest(id, setup, 900_000);
  check("the pinned build, the test extension and two checkouts are on the volume", installed.code === 0 && installed.text.includes("pinned-installed"), installed.text.slice(-400));
  for (const step of ["stop", "start"]) {
    const reply = await api("POST", `/api/workspaces/${id}/${step}`);
    check(`the workspace takes ${step}`, reply.status === 202 || reply.status === 200, `${reply.status} ${reply.body.slice(0, 160)}`);
    await until(
      `the workspace to finish ${step}`,
      async () => {
        const w = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.id === id);
        return w && !w.busy && w.status === (step === "stop" ? "stopped" : "running") ? w : false;
      },
      300_000,
      2000,
    );
  }
  // The server's own answer to /version when it gives one, else its process list: the pinned build's folder is in its command line.
  const running_build = async () => {
    const r = await guest(id, `ps -eo args= | grep -F 'servers/${commit}/' | grep -v grep | head -n 1 | cut -c1-200`, 60_000);
    return r.text.trim();
  };
  const serving = await until("the editor server to run the pinned build", async () => (await running_build()) || false, 180_000, 3000).catch(() => false);
  check("the editor server in the guest runs Microsoft's pinned build", Boolean(serving), `process: ${serving}; /version says ${await serverNow()}; pinned ${commit}`);

  // 1. Open the window from the Connect step: one explorer root per checkout.
  await connectFromTheMainWindow();
  const editor = await page(editorPort, /^https?:\/\/[^/]*\.localhost:\d+\//, 180_000);
  const opened = await explorerOf(editor, "first-open");
  await picture(editor, "first-open");
  const roots = opened.rows.filter((r) => r.level === "1").map((r) => r.name);
  // The multi-root file lists the checkouts (the volume itself only when loose files sit beside them): api and web, nothing else.
  check("the window shows one explorer root per checkout", JSON.stringify([...roots].sort()) === JSON.stringify(["api", "web"]), JSON.stringify(opened.rows));
  check("the window opened the multi-root file the attach wrote", /\.puddle\/editor\/[^/]+\.code-workspace/.test(opened.where.search), JSON.stringify(opened.where));

  // 2. The test extension stores its session in the editor's secret storage when it first runs.
  const stored = await until("the canary session to be stored", async () => (await canaryLog()).match(/ stored (canary-\S+)/)?.[1] ?? false, 120_000, 3000).catch(() => undefined);
  check("the test extension stored a canary session in secret storage", stored !== undefined, (await canaryLog()).slice(-300));
  editor.close();

  // 3. Close the window the way the close button does, then open it again from the Connect step.
  const closed = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", join(here, "window-close.ps1"), "-Title", `^${NAME} - `], { encoding: "utf8" });
  check("the editor window is closed like the close button", closed.status === 0, closed.stdout.trim());
  await until("the editor window to be gone", async () => !(await pageTarget(editorPort, /\.localhost:\d+\//).catch(() => undefined)), 60_000, 1000).catch(() => {});
  // The shell's last-window hook ends the extension hosts; wait for that, or the reopen could meet the old host.
  await sleep(10_000);
  await connectFromTheMainWindow();
  const again = await page(editorPort, /^https?:\/\/[^/]*\.localhost:\d+\//, 180_000);
  const reopened = await explorerOf(again, "reopen");
  await picture(again, "reopen");
  const kept = await until("the extension host of the reopened window to read its secret", async () => (await canaryLog()).split("\n").find((l) => / found /.test(l)) ?? false, 180_000, 3000).catch(() => undefined);
  check("the reopened window's extension finds the same session in secret storage (still signed in)", kept?.includes(` found ${stored} `) === true, (await canaryLog()).slice(-400));
  const rootsAgain = reopened.rows.filter((r) => r.level === "1").map((r) => r.name);
  check("the reopened window shows the same roots", JSON.stringify(rootsAgain) === JSON.stringify(roots), JSON.stringify(reopened.rows));
  again.close();


  // 4. Three workspaces, three editor windows at once. The first is open from step 3.
  const more = [
    { name: "msedit2", checkouts: ["alpha", "beta"] },
    { name: "msedit3", checkouts: ["gamma", "delta"] },
  ];
  const all = [{ name: NAME, id, checkouts: ["api", "web"], window: await editorOf(id, 0), slot: 0 }];
  for (const [i, w] of more.entries()) {
    w.id = await createRunning(w.name);
    const dir = await waitOwnInstall(w.id);
    const put = await putPinned(w.id, dir, w.checkouts);
    check(`${w.name}: the pinned build and its checkouts are on the volume`, put.code === 0 && put.text.includes("pinned-installed"), put.text.slice(-300));
    await restart(w.id);
    await connectFromTheMainWindow(w.name);
    w.slot = i + 1;
    w.window = await editorOf(w.id, w.slot);
    all.push(w);
  }
  check("three workspaces have three different ids", new Set(all.map((w) => w.id)).size === 3, JSON.stringify(all.map((w) => w.id)));
  for (const w of all) {
    const shown = await explorerOf(w.window, `${w.name}-window`);
    await picture(w.window, `${w.name}-roots`);
    const rootNames = shown.rows.filter((r) => r.level === "1").map((r) => r.name);
    check(`${w.name}: its window shows its own roots and no other workspace's`, JSON.stringify([...rootNames].sort()) === JSON.stringify([...w.checkouts].sort()), JSON.stringify(shown.rows));
    check(`${w.name}: its window is on its own origin`, shown.where.href.includes(`${w.id}.localhost`), shown.where.href);
  }
  // A command typed in each window's terminal runs in that window's own guest.
  for (const w of all) {
    await typeInTerminal(w.window, `echo "$(hostname) ${w.name}" > /tmp/window-who`);
  }
  const hostnames = [];
  for (const w of all) {
    const said = await until(`the command typed in ${w.name}'s window to run in its guest`, async () => (await guest(w.id, "cat /tmp/window-who 2>/dev/null", 60_000)).text.trim() || false, 120_000, 3000).catch(() => "");
    const own = (await guest(w.id, "hostname", 60_000)).text.trim();
    hostnames.push(own);
    check(`${w.name}: the window's terminal ran in its own guest (hostname ${own})`, said === `${own} ${w.name}`, JSON.stringify(said));
  }
  check("the three guests have three different hostnames", new Set(hostnames).size === 3, JSON.stringify(hostnames));

  // Closing all three windows: no editor WebView2 process of these workspaces, no extension host in any guest.
  for (const w of all) w.window.close();
  for (const w of all) {
    const closedOne = spawnSync("powershell", ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", join(here, "window-close.ps1"), "-Title", `^${w.name} - `], { encoding: "utf8" });
    check(`${w.name}: the editor window is closed like the close button`, closedOne.status === 0, closedOne.stdout.trim());
  }
  const left = await until("no editor WebView2 process of the three workspaces", () => editorWebViewProcesses(all.map((w) => w.id)).length === 0, 90_000, 2000).then(
    () => [],
    () => editorWebViewProcesses(all.map((w) => w.id)),
  );
  check("after closing all three windows no editor WebView2 process remains", left.length === 0, `pids ${left.join(",")}`);
  for (const w of all) {
    const hosts = async () => (await guest(w.id, "ps -eo args= | grep -c '[e]xtensionHost'", 60_000)).text.trim();
    const ended = await until(`the extension hosts of ${w.name} to end`, async () => (await hosts()) === "0", 90_000, 3000).then(() => true, () => false);
    check(`${w.name}: its guest has no extension host left`, ended, `extensionHost processes: ${await hosts()}`);
  }

  // Stop the workspaces, as the user does at the end (the data folder is a temp one and goes with the runner).
  for (const w of all) await api("POST", `/api/workspaces/${w.id}/stop`);
} catch (err) {
  check("the run ran to the end", false, String(err?.stack ?? err));
} finally {
  writeFileSync(join(out, "app.log"), appLog);
  if (id) {
    const logs = await guest(
      id,
      "tail -n 60 /run/puddle/agent.log; grep -rn -i canary /workspace/.puddle/vscode-server/server-data/data/logs 2>/dev/null | tail -n 20 | cut -c1-300; ls -la /workspace/.puddle/vscode-server/extensions; for f in $(find /workspace/.puddle/vscode-server/server-data -name '*.log' 2>/dev/null | sort | tail -n 8); do echo \"== $f\"; tail -n 30 \"$f\"; done; echo == canary; cat /tmp/canary-secret.log 2>/dev/null; true",
      60_000,
    ).catch(() => ({ text: "" }));
    writeFileSync(join(out, "guest-logs.txt"), logs.text);
  }
  uia?.close();
  if (!appExited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\nall checks passed");
// The app's microVM processes outlive it and hold its output pipes, and the event loop waits for those: exit by hand
// (the run that passed every check sat 48 more minutes until the step timeout).
process.exit(0);
