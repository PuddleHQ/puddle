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
//
// Why the pinned build is put on the volume by this script: the product installs the newest build by itself, and
// this run must not change when Microsoft ships; the script waits for that install, then replaces the build the
// editor starts with the pinned one and restarts the workspace. Automatic updates are switched off for the run.
//
// Environment: PUDDLE_APP_EXE (a debug build without the `fixture` feature, with `runtime/` beside it),
// PUDDLE_AGENT_BIN (the guest agent), MS_EDITOR_PIN (ci/vscode-server.sha256), MS_EDITOR_OUT (a folder for pictures and
// logs), CDP_PORT (default 9340). Plain Node 24, no packages.
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

const EXPLORER_READY = "document.querySelectorAll('.explorer-folders-view .monaco-list-row .label-name').length >= 3";
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

/** Presses Connect, then "Open in the browser", in the main window as a person does. */
async function connectFromTheMainWindow() {
  const main = await page(mainPort, /^http:\/\/127\.0\.0\.1:\d+\//);
  try {
    const clicked = (script) => main.evaluate(`(() => { ${script} })()`, true);
    await clicked(`document.querySelector('nav[aria-label="Main"] a[href="/workspaces"]')?.click(); return true;`);
    await until(
      "the Connect button of the workspace",
      () => main.evaluate(`[...document.querySelectorAll('button')].some((b) => b.textContent.replace(/\\s+/g, ' ').trim() === 'Connect to ${NAME}')`),
      60_000,
      500,
    );
    await clicked(`[...document.querySelectorAll('button')].find((b) => b.textContent.replace(/\\s+/g, ' ').trim() === 'Connect to ${NAME}').click(); return true;`);
    await until(
      "the Connect step's browser button",
      () => main.evaluate(`[...document.querySelectorAll('button')].some((b) => b.textContent.trim() === 'Open in the browser')`),
      30_000,
      300,
    );
    await clicked(`[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === 'Open in the browser').click(); return true;`);
  } finally {
    main.close();
  }
}

const canaryLog = async () => (await guest(id, "cat /tmp/canary-secret.log 2>/dev/null; true", 60_000)).text;
const serverNow = () => guest(id, "curl -s --max-time 3 http://127.0.0.1:41337/version", 60_000).then((r) => r.text.trim());

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

  // The product's own install of the newest build comes first; wait for it so it cannot overwrite the pinned build later.
  const builds = `/workspaces/${id}/.puddle/vscode-server/servers`;
  const own = await until(
    "the editor's own install of Microsoft's newest build",
    async () => {
      const r = await guest(id, `cat ${builds}/current 2>/dev/null`, 60_000);
      return /^[0-9a-f]{40}$/.test(r.text.trim()) ? r.text.trim() : false;
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
  const data = `/workspaces/${id}/.puddle/vscode-server`;
  const extension = `${data}/extensions/puddle-test.canary-secret-0.0.1`;
  const setup = [
    "set -eu",
    `builds=${builds}; commit=${commit}; tmp=$builds/$commit.tar.gz.partial`,
    `curl -fsSL --retry 3 --retry-delay 5 -o "$tmp" "https://update.code.visualstudio.com/commit:$commit/server-linux-x64-web/stable"`,
    `got=$(sha256sum "$tmp" | cut -d' ' -f1)`,
    `[ "$got" = ${sha256} ] || { echo "checksum mismatch: $got"; exit 1; }`,
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
    `cd /workspaces/${id}`,
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
  const serving = await until("the editor server to serve the pinned build", async () => (await serverNow()) === commit, 180_000, 3000).catch(() => false);
  check("the editor server in the guest runs Microsoft's pinned build", serving, `serving ${await serverNow()}, pinned ${commit}`);

  // 1. Open the window from the Connect step: one explorer root per checkout.
  await connectFromTheMainWindow();
  const editor = await page(editorPort, /^https?:\/\/[^/]*\.localhost:\d+\//, 180_000);
  const opened = await explorerOf(editor, "first-open");
  await picture(editor, "first-open");
  const roots = opened.rows.filter((r) => r.level === "1").map((r) => r.name);
  check("the window shows one explorer root per checkout", roots.includes("api") && roots.includes("web") && roots.length === 3, JSON.stringify(opened.rows));
  check("the window opened the multi-root file the attach wrote", opened.where.search.includes(`.puddle/editor/${id}.code-workspace`), JSON.stringify(opened.where));

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

  // Stop the workspace, as the user does at the end (the data folder is a temp one and goes with the runner).
  await api("POST", `/api/workspaces/${id}/stop`);
} catch (err) {
  check("the run ran to the end", false, String(err?.stack ?? err));
} finally {
  writeFileSync(join(out, "app.log"), appLog);
  if (id) {
    const logs = await guest(
      id,
      "tail -n 60 /run/puddle/agent.log; for f in $(find /workspaces/*/.puddle/vscode-server/server-data -name '*.log' 2>/dev/null | sort | tail -n 8); do echo \"== $f\"; tail -n 30 \"$f\"; done; echo == canary; cat /tmp/canary-secret.log 2>/dev/null; true",
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
