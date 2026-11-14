// SPDX-License-Identifier: AGPL-3.0-or-later
// The WebView2 half of the terminal's key assumptions,
// checked with real input: Win32 SendInput through `terminal-keys.ps1`, because DevTools' own key
// events skip the accelerators and the webview's key handling, which are what is in question.
//
// Session 1, the page `terminal-keys.html` in a workspace window (the shell's probe window, whose
// webview has the browser accelerator keys off, as the terminal screen will give the main window), driven over
// WebView2's DevTools Protocol like sandbox-window.mjs:
//   - Set-Clipboard, then Ctrl+Shift+V and then Shift+Insert each fire `paste` with that text;
//   - Ctrl+R and F5, with the keydown cancelled, do not reload the document, and the page saw both
//     keys, so a missing reload is not a missing key.
// Session 2, the main window alone: Ctrl+Q with no terminal open quits at once (exit 0). The main window's
// menu is hidden, so the host takes the key from the webview itself and keeps it from the page.
// Session 3, the main window with the fixture saying two terminals are open (PUDDLE_APP_FIXTURE_TERMINALS):
// Ctrl+Q opens the shell's "Quit Puddle" question and does not quit; Escape cancels it and the app runs on.
// The page gets the OS focus the way a user gives it: a real mouse click in the page.
//
// Environment: PUDDLE_APP_EXE (a debug build with all features: the in-memory fixture backend),
// CDP_PORT (default 9340; sessions 2 and 3 use the next two). Plain Node 24, no packages.
import { spawn, spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { randomBytes } from "node:crypto";
import { connect } from "./cdp.mjs";
import { onChildEnd } from "./child-end.mjs";

const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
const basePort = Number(process.env.CDP_PORT ?? 9340);
const helper = fileURLToPath(new URL("./terminal-keys.ps1", import.meta.url));

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

/** Polls `probe` until it returns something truthy, for `within` ms; no fixed sleeps. */
async function until(what, probe, within = 60_000) {
  const deadline = Date.now() + within;
  for (;;) {
    const value = await probe().catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(250);
  }
}

/** Runs one action of the SendInput helper; the helper throws on a failed call, which shows here. */
function input(action, ...args) {
  const run = spawnSync("pwsh", ["-NoProfile", "-File", helper, action, ...args], { encoding: "utf8", timeout: 60_000 });
  if (run.status !== 0) throw new Error(`input ${action} ${args.join(" ")} failed (${run.status}): ${run.stderr || run.error}`);
  return run.stdout.trim();
}

const listPages = async (port) => (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).filter((t) => t.type === "page");

function start(env) {
  const app = spawn(exe, [], { stdio: ["ignore", "inherit", "pipe"], env: { ...process.env, PUDDLE_APP_FIXTURE: "1", ...env } });
  const state = { app, exited: false, code: undefined, log: "" };
  app.stderr.on("data", (chunk) => {
    state.log += chunk;
    process.stderr.write(chunk);
  });
  state.done = new Promise((resolve) =>
    onChildEnd(app, (code) => {
      state.exited = true;
      state.code = code;
      resolve();
    }),
  );
  return state;
}

function stop(state) {
  if (!state.exited) spawnSync("taskkill", ["/PID", String(state.app.pid), "/T", "/F"], { stdio: "ignore" });
}

/** A real click in the middle of `selector`'s box, so the window takes the OS focus and the element the page's. */
async function clickIn(page, state, selector) {
  const spot = await page.evaluate(`(() => {
    const r = document.querySelector(${JSON.stringify(selector)}).getBoundingClientRect();
    return { x: Math.round(window.screenX + r.left + r.width / 2), y: Math.round(window.screenY + r.top + r.height / 2), dpr: window.devicePixelRatio };
  })()`);
  // screenX/Y are in CSS pixels; SendInput wants physical ones.
  const x = Math.round(spot.x * spot.dpr);
  const y = Math.round(spot.y * spot.dpr);
  input("click", String(x), String(y));
  const focused = await until("the page to have focus", () => page.evaluate("document.hasFocus()"), 10_000).catch(() => false);
  if (!focused) {
    console.log(`     click at ${x},${y} (css ${spot.x},${spot.y}, dpr ${spot.dpr}) did not focus the page; windows:\n${input("windows", String(state.app.pid))}`);
  }
  return focused;
}

const scratchPage = await readFile(new URL("./terminal-keys.html", import.meta.url));
const server = createServer((_req, res) => {
  res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
  res.end(scratchPage);
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const sitePort = server.address().port;
const workspaceId = `k${randomBytes(5).toString("hex").slice(0, 7)}`;

let first;
let second;
let third;
let page;
try {
  // Session 1: the keys.
  const keyPort = basePort;
  first = start({
    PUDDLE_APP_SANDBOX_PROBE: `http://${workspaceId}.localhost:${sitePort}/`,
    PUDDLE_APP_SANDBOX_DEBUG_PORT: String(keyPort),
    PUDDLE_APP_SANDBOX_PROBE_WORKSPACE: workspaceId,
  });
  const target = await until("the probe window", async () => {
    if (first.exited) throw new Error("puddle-app exited early");
    return (await listPages(keyPort)).find((t) => t.url.startsWith(`http://${workspaceId}.localhost:${sitePort}/`));
  });
  page = await connect(target.webSocketDebuggerUrl);
  await until("the page to load", () => page.evaluate("document.readyState === 'complete' && typeof window.probe === 'object'"));
  const documentId = await page.evaluate("window.probe.document");
  check("a click in the page gives it the focus", await clickIn(page, first, "#input"));
  check("the textarea has the page's focus", await page.evaluate("document.activeElement === document.getElementById('input')"));

  const text = "echo pasted from the clipboard";
  input("clipboard", text);
  const pastes = () => page.evaluate("window.probe.pastes");
  input("keys", "ctrl+shift+v");
  const afterCtrlShiftV = await until("a paste from Ctrl+Shift+V", async () => (await pastes()).length >= 1 && (await pastes()), 10_000).catch(() => pastes());
  check("Ctrl+Shift+V fires paste with the clipboard's text", afterCtrlShiftV[0] === text, JSON.stringify(afterCtrlShiftV));
  input("keys", "shift+insert");
  const afterShiftInsert = await until("a paste from Shift+Insert", async () => (await pastes()).length >= 2 && (await pastes()), 10_000).catch(() => pastes());
  check("Shift+Insert fires paste with the clipboard's text", afterShiftInsert[1] === text, JSON.stringify(afterShiftInsert));

  input("keys", "ctrl+r");
  input("keys", "f5");
  const keys = await until("both keys in the page's log", async () => {
    const log = await page.evaluate("window.probe.keys");
    return log.some((k) => k.key === "F5") && log.some((k) => k.ctrl && k.key.toLowerCase() === "r") && log;
  }, 10_000).catch(() => page.evaluate("window.probe.keys"));
  check("Ctrl+R reached the page, its default cancelled", keys.some((k) => k.ctrl && k.key.toLowerCase() === "r" && k.defaultPrevented), JSON.stringify(keys));
  check("F5 reached the page, its default cancelled", keys.some((k) => k.key === "F5" && k.defaultPrevented), JSON.stringify(keys));
  // A reload starts within a moment of the key; give it two seconds, then read the document's own id.
  await sleep(2000);
  const after = await page.evaluate("window.probe.document").catch((err) => `unreadable: ${err.message}`);
  check("Ctrl+R and F5 with the keydown cancelled do not reload the document", after === documentId, `${documentId} -> ${after}`);
  page.close();
  page = undefined;
  stop(first);
  await first.done;

  // Session 2: Ctrl+Q on the main window.
  const mainPort = basePort + 1;
  second = start({ PUDDLE_APP_DEBUG_PORT: String(mainPort) });
  const mainTarget = await until("the main window", async () => {
    if (second.exited) throw new Error("puddle-app exited early");
    return (await listPages(mainPort)).find((t) => /^http:\/\/127\.0\.0\.1:\d+\//.test(t.url));
  });
  page = await connect(mainTarget.webSocketDebuggerUrl);
  await until("the main page to load", () => page.evaluate("document.readyState === 'complete' && !!document.body"));
  const focused = await clickIn(page, second, "body");
  check("a click in the main window gives it the focus", focused);
  await page.evaluate(`window.seenKeys = []; addEventListener("keydown", (e) => window.seenKeys.push(e.key + (e.ctrlKey ? "+ctrl" : "")), true); true`);
  input("keys", "ctrl+q");
  await Promise.race([second.done, sleep(15_000)]);
  check("Ctrl+Q with no terminal open quits the program with exit 0", second.exited && second.code === 0, `exited: ${second.exited}, code: ${second.code}`);
  page.close();
  page = undefined;
  stop(second);
  await second.done;

  // Session 3: Ctrl+Q with terminals open asks first.
  const askPort = basePort + 2;
  third = start({ PUDDLE_APP_DEBUG_PORT: String(askPort), PUDDLE_APP_FIXTURE_TERMINALS: "2" });
  const askTarget = await until("the main window", async () => {
    if (third.exited) throw new Error("puddle-app exited early");
    return (await listPages(askPort)).find((t) => /^http:\/\/127\.0\.0\.1:\d+\//.test(t.url));
  });
  page = await connect(askTarget.webSocketDebuggerUrl);
  await until("the main page to load", () => page.evaluate("document.readyState === 'complete' && !!document.body"));
  check("a click in the main window gives it the focus", await clickIn(page, third, "body"));
  await page.evaluate(`window.seenKeys = []; addEventListener("keydown", (e) => window.seenKeys.push(e.key + (e.ctrlKey ? "+ctrl" : "")), true); true`);
  input("keys", "ctrl+q");
  const questionUp = () => input("windows", String(third.app.pid)).includes('"Quit Puddle"');
  const asked = await until("the quit question", async () => questionUp(), 15_000).catch(() => false);
  check("Ctrl+Q with terminals open shows the quit question", asked, input("windows", String(third.app.pid)));
  check("the program is still running while the question is up", !third.exited);
  const seen = await page.evaluate("window.seenKeys").catch((err) => err.message);
  check("the page did not get the q of Ctrl+Q", !JSON.stringify(seen).includes("q+ctrl"), JSON.stringify(seen));
  input("keys", "esc");
  const gone = await until("the question to close", async () => !questionUp(), 15_000).catch(() => false);
  check("Escape cancels the question", gone, input("windows", String(third.app.pid)));
  await sleep(1500);
  check("cancelling leaves the program running", !third.exited, `exited: ${third.exited}, code: ${third.code}`);
} catch (err) {
  check("the test ran to the end", false, String(err?.stack ?? err));
} finally {
  page?.close();
  for (const state of [first, second, third]) if (state) stop(state);
  server.close();
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\nterminal keys test passed");
process.exit(0);
