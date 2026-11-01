// SPDX-License-Identifier: GPL-3.0-or-later
// Smoke test of a workspace window in the real shell (WebView2, so Windows only): the shell opens
// one on a page this script serves from `probe.localhost`, standing in for a workspace's editor,
// and the page attacks the shell the way hostile guest script would. Driven over WebView2's DevTools
// Protocol like smoke.mjs, plain Node 24, no packages.
//
// What it shows, in the window and in one the page opens: no Tauri command succeeds, a navigation or
// window.open to another site goes to the default browser (the shell logs it instead of starting a
// browser in this test) and leaves the window where it was, the guest's /proxy/<port>/ links open a
// guarded window on that port's own origin (<port>.probe.localhost) with the same result, and the
// editor's "New Window" becomes a second guarded window with the same result.
// What it does not show: clipboard prompts, zoom keys, the browser shortcuts being off, and a
// close ending the workspace's extension hosts (unit tests cover the decisions, nobody has watched
// the rest in a window).
//
// Environment: PUDDLE_APP_EXE (a debug build with all features: the in-memory fixture backend),
// CDP_PORT (default 9334).
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { setTimeout as sleep } from "node:timers/promises";

const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
const cdpPort = Number(process.env.CDP_PORT ?? 9334);
const attackPage = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "attack.html"));

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

// The stand-in editor: every path answers with the attack page.
const server = createServer((_req, res) => {
  res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
  res.end(attackPage);
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const sitePort = server.address().port;
const origin = `http://probe.localhost:${sitePort}`;
const probePage = `${origin}/attack.html`;
const onPage = (t) => t.url.startsWith(probePage);

const scratch = mkdtempSync(join(tmpdir(), "puddle-sandbox-window-"));
const env = {
  ...process.env,
  PUDDLE_APP_FIXTURE: "1",
  PUDDLE_APP_DATA_DIR: join(scratch, "data"),
  PUDDLE_APP_SANDBOX_PROBE: probePage,
  PUDDLE_APP_SANDBOX_DEBUG_PORT: String(cdpPort),
};
const app = spawn(exe, [], { stdio: ["ignore", "inherit", "pipe"], env });
let appLog = "";
app.stderr.on("data", (chunk) => {
  appLog += chunk;
  process.stderr.write(chunk);
});
let appExited = false;
app.on("exit", () => {
  appExited = true;
});

async function targets() {
  const list = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
  return list.filter((t) => t.type === "page" && t.url.startsWith(origin));
}

// The windows on a forwarded port's origin (`<port>.probe.localhost`). They have a data folder of
// their own, so a WebView2 browser process of their own, which the shell gives the next DevTools port.
async function forwardTargets(port) {
  const list = await (await fetch(`http://127.0.0.1:${cdpPort + 1}/json/list`)).json();
  return list.filter((t) => t.type === "page" && t.url.startsWith(`http://${port}.probe.localhost:${sitePort}/`));
}

// The workspace pages once `done(pages)` holds. A window is listed from its first navigation on,
// so a window that is still on its way to a page shows up at the address it started from.
async function waitForTargets(what, done) {
  let seen = [];
  for (let i = 0; i < 150; i++) {
    if (appExited) throw new Error("puddle-app exited early");
    seen = await targets().catch(() => []);
    if (done(seen)) return seen;
    await sleep(400);
  }
  throw new Error(`expected ${what}, saw ${JSON.stringify(seen.map((t) => t.url))}`);
}

function connect(url) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(url);
    let next = 1;
    const pending = new Map();
    ws.onmessage = (e) => {
      const m = JSON.parse(e.data);
      const p = pending.get(m.id);
      if (p) {
        pending.delete(m.id);
        m.error ? p.reject(new Error(JSON.stringify(m.error))) : p.resolve(m.result);
      }
    };
    ws.onerror = () => reject(new Error("DevTools websocket failed"));
    ws.onopen = () =>
      resolve({
        // `gesture`: run as if the user had clicked, so window.open is not blocked as a pop-up.
        evaluate: async (expression, gesture = false) => {
          const id = next++;
          const done = new Promise((res, rej) => pending.set(id, { resolve: res, reject: rej }));
          ws.send(JSON.stringify({ id, method: "Runtime.evaluate", params: { expression, returnByValue: true, awaitPromise: true, userGesture: gesture } }));
          const r = await done;
          if (r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails));
          return r.result.value;
        },
        close: () => ws.close(),
      });
  });
}

async function attack(target, label) {
  const page = await connect(target.webSocketDebuggerUrl);
  try {
    // The target is listed as soon as the navigation commits, a moment before the page's script
    // has run: wait for the attack page itself (a missing page still fails, after the wait).
    for (let i = 0; i < 50 && (await page.evaluate(`typeof window.__attackRun`)) !== "function"; i++) await sleep(200);
    const results = await page.evaluate("window.__attackRun()");
    const bridge = results.find((r) => r.name === "bridge");
    console.log(`     ${label}: IPC bridge present: ${bridge?.detail}`);
    for (const r of results.filter((x) => x.name !== "bridge")) {
      check(`${label}: ${r.name} does not succeed`, !r.succeeded, r.detail.split("\n")[0]);
    }
    check(`${label}: every attack ran`, results.length >= 10, `${results.length} attempts`);
    // The editor's own scripts (VS Code assigns `toString` on plain objects, in strict mode) and a
    // forwarded dev server's must run, so these windows keep Object.prototype writable.
    const writable = await page.evaluate(`(() => { "use strict"; try { const o = {}; o.toString = () => "ok"; return "writable"; } catch (e) { return String(e); } })()`);
    check(`${label}: a page can assign toString on a plain object`, writable === "writable", writable);
  } finally {
    page.close();
  }
}

const logged = (pattern) => pattern.test(appLog);
async function waitForLog(pattern, ms = 15_000) {
  for (let i = 0; i < ms / 300 && !logged(pattern); i++) await sleep(300);
  return logged(pattern);
}

let main;
try {
  // The shell opens the window on the workspace's origin and then sends it to the page.
  const opened = await waitForTargets("the workspace window on its page", (pages) => pages.some(onPage));
  const first = opened.find(onPage);
  check("the shell opened one window, on the workspace page", opened.length === 1, JSON.stringify(opened.map((t) => t.url)));
  check("the window got a data folder of its own", existsSync(join(scratch, "data", "webview", "probe")));
  await attack(first, "workspace window");

  main = await connect(first.webSocketDebuggerUrl);
  // A navigation to another site is cancelled and handed to the default browser.
  await main.evaluate(`setTimeout(() => { location.href = "https://example.org/navigated"; }, 0); true`);
  check("a navigation to another site is opened in the browser", await waitForLog(/opened in the browser.*example\.org\/navigated/));
  check("and the window stays on the workspace page", (await targets()).some(onPage));
  // window.open: another site goes to the browser and opens no window; so does a /proxy/ link that
  // names no port.
  await main.evaluate(`window.open("https://example.org/popup"); true`, true);
  check("window.open to another site is opened in the browser", await waitForLog(/opened in the browser.*example\.org\/popup/));
  await main.evaluate(`window.open("/proxy/web/"); true`, true);
  check("a /proxy/ link without a port is opened in the browser", await waitForLog(/opened in the browser.*\/proxy\/web\//));
  await sleep(1000);
  check("neither opened a window", (await targets()).length === 1, JSON.stringify((await targets()).map((t) => t.url)));
  // The guest's port proxy: a guarded window on Puddle's own forward of the port, not the browser.
  await main.evaluate(`window.open("/proxy/5173/app"); true`, true);
  let forwarded = [];
  for (let i = 0; i < 50 && forwarded.length === 0; i++) {
    forwarded = await forwardTargets(5173).catch(() => []);
    if (forwarded.length === 0) await sleep(300);
  }
  check("a /proxy/<port>/ link opens a window on the port's own origin", forwarded.length === 1, JSON.stringify(forwarded.map((t) => t.url)));
  check("and not the default browser", !logged(/opened in the browser.*\/proxy\/5173\//));
  if (forwarded.length === 1) await attack(forwarded[0], "forwarded window");
  // The editor's "New Window": a second window of the same kind.
  await main.evaluate(`window.open("/attack.html?popup=1"); true`, true);
  const isPopup = (t) => t.url.startsWith(`${probePage}?popup=1`);
  const both = await waitForTargets("a second workspace window", (pages) => pages.some(isPopup));
  const popup = both.find(isPopup);
  check("the editor's New Window opens a second workspace window", both.length === 2, JSON.stringify(both.map((t) => t.url)));
  await attack(popup, "second window");
} catch (err) {
  check("the sandbox window test ran to the end", false, String(err?.stack ?? err));
} finally {
  main?.close();
  if (!appExited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
  server.close();
  // WebView2's child processes can still hold the data folder for a moment after the kill; a
  // leftover temp folder is not a result of this test.
  try {
    rmSync(scratch, { recursive: true, force: true, maxRetries: 10, retryDelay: 500 });
  } catch (err) {
    console.log(`note: could not remove ${scratch}: ${err.code ?? err}`);
  }
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\nsandbox window test passed");
process.exit(0);
