// SPDX-License-Identifier: GPL-3.0-or-later
// Smoke test of a workspace window in the real shell (WebView2, so Windows only): the shell opens
// one on a page this script serves from `probe.localhost`, standing in for a workspace's editor,
// and the page attacks the shell the way hostile guest script would. Driven over WebView2's DevTools
// Protocol like smoke.mjs, plain Node 24, no packages.
//
// What it shows, in the window and in one the page opens: no Tauri command succeeds, a navigation or
// window.open to another site goes to the default browser (the shell logs it instead of starting a
// browser in this test) and leaves the window where it was, the guest's /proxy/ links go to the
// browser, and the editor's "New Window" becomes a second guarded window with the same result.
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

const scratch = mkdtempSync(join(tmpdir(), "puddle-sandbox-window-"));
const env = {
  ...process.env,
  PUDDLE_APP_FIXTURE: "1",
  PUDDLE_APP_DATA_DIR: join(scratch, "data"),
  PUDDLE_APP_SANDBOX_PROBE: `${origin}/attack.html`,
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

async function waitForTargets(count) {
  let seen = [];
  for (let i = 0; i < 150; i++) {
    if (appExited) throw new Error("puddle-app exited early");
    seen = await targets().catch(() => []);
    if (seen.length >= count) return seen;
    await sleep(400);
  }
  throw new Error(`expected ${count} workspace page(s), saw ${JSON.stringify(seen.map((t) => t.url))}`);
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
    const results = await page.evaluate("window.__attackRun()");
    const bridge = results.find((r) => r.name === "bridge");
    console.log(`     ${label}: IPC bridge present: ${bridge?.detail}`);
    for (const r of results.filter((x) => x.name !== "bridge")) {
      check(`${label}: ${r.name} does not succeed`, !r.succeeded, r.detail.split("\n")[0]);
    }
    check(`${label}: every attack ran`, results.length >= 10, `${results.length} attempts`);
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
  const [first] = await waitForTargets(1);
  check("the shell opened a window on the workspace origin", first.url.startsWith(`${origin}/attack.html`), first.url);
  check("the window got a data folder of its own", existsSync(join(scratch, "data", "webview", "probe")));
  await attack(first, "workspace window");

  main = await connect(first.webSocketDebuggerUrl);
  // A navigation to another site is cancelled and handed to the default browser.
  await main.evaluate(`setTimeout(() => { location.href = "https://example.org/navigated"; }, 0); true`);
  check("a navigation to another site is opened in the browser", await waitForLog(/opened in the browser.*example\.org\/navigated/));
  check("and the window stays on the workspace page", (await targets()).some((t) => t.url.startsWith(`${origin}/attack.html`)));
  // window.open: another site, and the guest's port proxy, both go to the browser and open no window.
  await main.evaluate(`window.open("https://example.org/popup"); true`, true);
  check("window.open to another site is opened in the browser", await waitForLog(/opened in the browser.*example\.org\/popup/));
  await main.evaluate(`window.open("/proxy/5173/app"); true`, true);
  check("a /proxy/ link is opened in the browser", await waitForLog(/opened in the browser.*\/proxy\/5173\/app/));
  await sleep(1000);
  check("neither opened a window", (await targets()).length === 1, JSON.stringify((await targets()).map((t) => t.url)));
  // The editor's "New Window": a second window of the same kind.
  await main.evaluate(`window.open("/attack.html?popup=1"); true`, true);
  const both = await waitForTargets(2);
  const popup = both.find((t) => t.url.includes("popup=1"));
  check("the editor's New Window opens a second workspace window", !!popup, JSON.stringify(both.map((t) => t.url)));
  if (popup) await attack(popup, "second window");
} catch (err) {
  check("the sandbox window test ran to the end", false, String(err?.stack ?? err));
} finally {
  main?.close();
  if (!appExited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
  server.close();
  rmSync(scratch, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\nsandbox window test passed");
process.exit(0);
