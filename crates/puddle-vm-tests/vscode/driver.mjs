// SPDX-License-Identifier: GPL-3.0-or-later
// VS Code half of the desktop VS Code client (crates/puddle-vm-tests/src/vscode.rs).
//
// Drives a desktop VS Code that the test started with `--remote-debugging-port`, over Electron's DevTools Protocol, with
// Playwright's `connectOverCDP`. JSON lines on stdin; every command gets one JSON line on stdout, in the shape of the browser
// client (`browser/driver.mjs`): {"ok":true,"result":...,"console":[],"pageErrors":[]} or {"ok":false,"error":"..."}.
//   {"op":"connect","port":9230,"timeoutMs":..}   connects to the window; retries a refused connection until the budget ends
//   {"op":"attached","ready":"<expr>","timeoutMs":..,"fresh":bool}
//       waits until the workbench shows a folder in the explorer and the remote indicator says `SSH:` (and, with `fresh`, until
//       the page is not the one `mark` stamped), then returns {folders, remote, title}
//   {"op":"mark"}                                  stamps the current page, so `attached` with `fresh` can tell a reload from before
//   {"op":"palette","label":"..","timeoutMs":..}   F1, types the label, clicks the row whose label is exactly that
//   {"op":"terminal","line":"..","timeoutMs":..}   opens a new terminal (taking the first folder when it asks which) and types `line`
//   {"op":"eval","expression":".."}                evaluates an expression in the window
//   {"op":"screenshot","path":".."}                saves a PNG of the window
//   {"op":"quit"}                                  disconnects (the test stops the VS Code process itself)
// Playwright comes from the UI's lockfile; PUDDLE_VM_BROWSER_MODULES names the node_modules folder that holds `playwright-core`.
import { createRequire } from "node:module";
import { join } from "node:path";
import { createInterface } from "node:readline";

const write = (body) => process.stdout.write(`${JSON.stringify(body)}\n`);

let browser;
const logs = [];
const errors = [];

// The explorer's top-level rows and the remote indicator: what "the window is attached and shows the workspace" means.
const FOLDERS = "Array.from(document.querySelectorAll('.explorer-folders-view .monaco-list-row .label-name')).map((e) => e.textContent.trim())";
const REMOTE = "(document.querySelector('[id=\"status.host\"]')?.textContent ?? '').trim()";

const workbench = () => {
  const found = browser.contexts().flatMap((c) => c.pages()).find((p) => /workbench/.test(p.url()));
  if (!found) throw new Error("no workbench window");
  return found;
};

const connect = async (command) => {
  const modules = process.env.PUDDLE_VM_BROWSER_MODULES;
  if (!modules) throw new Error("PUDDLE_VM_BROWSER_MODULES is not set");
  const { chromium } = createRequire(join(modules, "..", "x.js"))("playwright-core");
  const deadline = Date.now() + command.timeoutMs;
  for (;;) {
    try {
      // Code.exe opens its debug port a few seconds after it starts, and the workbench page a few after that:
      // a refused connection or a window with no workbench page yet means "not yet", so poll until the budget ends.
      browser = await chromium.connectOverCDP(`http://127.0.0.1:${command.port}`, { timeout: 5000 });
      workbench();
      break;
    } catch (error) {
      await browser?.close().catch(() => {});
      browser = undefined;
      if (Date.now() >= deadline) throw new Error(`no VS Code window on port ${command.port}: ${error.message}`);
      await new Promise((resolve) => setTimeout(resolve, 500));
    }
  }
  const page = workbench();
  page.on("console", (message) => logs.push(`${message.type()}: ${message.text()}`));
  page.on("pageerror", (error) => errors.push(String(error)));
  return null;
};

const attached = async (command) => {
  const page = workbench();
  const fresh = command.fresh ? "&& window.__puddleMark === undefined" : "";
  await page.waitForFunction(
    `${FOLDERS}.length > 0 && ${REMOTE}.includes('SSH:') ${fresh}`,
    undefined,
    { timeout: command.timeoutMs, polling: 250 },
  );
  return page.evaluate(`({ folders: ${FOLDERS}, remote: ${REMOTE}, title: document.title })`);
};

// The row whose label is exactly `label`: "Terminal: Create New Terminal" is a prefix of "... in Editor Area", and picking the
// wrong row is what broke the first hands-on run.
const exact = (label) => new RegExp(`^\\s*${label.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*$`);

const palette = async (label, timeoutMs) => {
  const page = workbench();
  await page.keyboard.press("F1");
  await page.locator(".quick-input-widget input").waitFor({ state: "visible", timeout: timeoutMs });
  await page.keyboard.insertText(label);
  await page.locator(".quick-input-list .monaco-list-row .label-name").filter({ hasText: exact(label) }).first().click({ timeout: timeoutMs });
  return null;
};

const PICKER = "(() => { const q = document.querySelector('.quick-input-widget'); return !!q && getComputedStyle(q).display !== 'none'; })()";
const TERMINAL = "!!document.querySelector('.terminal-wrapper .xterm-helper-textarea')";

const terminal = async (line, timeoutMs) => {
  const page = workbench();
  await palette("Terminal: Create New Terminal", timeoutMs);
  // In a multi-root workspace the command first asks which folder the terminal starts in; take the first one.
  const first = await page.waitForFunction(`${PICKER} ? 'picker' : ${TERMINAL} ? 'terminal' : false`, undefined, { timeout: timeoutMs, polling: 250 });
  if ((await first.jsonValue()) === "picker") await page.keyboard.press("Enter");
  await page.waitForFunction(`${TERMINAL} && !(${PICKER})`, undefined, { timeout: timeoutMs, polling: 250 });
  await page.locator(".terminal-wrapper .xterm-helper-textarea").first().focus({ timeout: timeoutMs });
  await page.keyboard.type(line);
  await page.keyboard.press("Enter");
  return null;
};

const run = async (command) => {
  switch (command.op) {
    case "connect":
      return connect(command);
    case "attached":
      return attached(command);
    case "mark":
      await workbench().evaluate("window.__puddleMark = 1");
      return null;
    case "palette":
      return palette(command.label, command.timeoutMs);
    case "terminal":
      return terminal(command.line, command.timeoutMs);
    case "eval":
      return workbench().evaluate(command.expression);
    case "screenshot":
      await workbench().screenshot({ path: command.path });
      return null;
    default:
      throw new Error(`unknown command ${command.op}`);
  }
};

const lines = createInterface({ input: process.stdin });
for await (const line of lines) {
  if (!line.trim()) continue;
  try {
    const command = JSON.parse(line);
    if (command.op === "quit") break;
    const result = await run(command);
    write({ ok: true, result, console: logs.splice(0), pageErrors: errors.splice(0) });
  } catch (error) {
    write({ ok: false, error: String(error?.message ?? error), console: logs.splice(0), pageErrors: errors.splice(0) });
  }
}
lines.close();
// connectOverCDP's close only disconnects: the VS Code process is the test's to stop.
await browser?.close().catch(() => {});
// stdin stays open after a quit, and an open stdin keeps node alive.
process.exit(0);
