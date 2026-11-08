// SPDX-License-Identifier: GPL-3.0-or-later
// VS Code half of the desktop VS Code client (crates/puddle-vm-tests/src/vscode.rs).
//
// Drives a desktop VS Code that the test started with `--remote-debugging-port`, over Electron's DevTools Protocol, with
// Playwright's `connectOverCDP`. JSON lines on stdin; every command gets one JSON line on stdout, in the shape of the browser
// client (`browser/driver.mjs`): {"ok":true,"result":...,"console":[],"pageErrors":[]} or {"ok":false,"error":"..."}.
//   {"op":"connect","port":9230,"timeoutMs":..}   connects to the window; retries a refused connection until the budget ends
//   {"op":"attached","timeoutMs":..}               waits until the workbench shows a folder in the explorer and the remote indicator
//                                                  says `SSH:`, then returns {folders, remote, title}
//   {"op":"reload","timeoutMs":..}                 Developer: Reload Window, then waits as `attached` does, on the new document
//   {"op":"palette","label":"..","timeoutMs":..}   F1, types the label, clicks the row whose label is exactly that
//   {"op":"terminal","line":"..","timeoutMs":..}   opens a new terminal (taking the first folder when it asks which) and types `line`
//   {"op":"eval","expression":".."}                evaluates an expression in the window
//   {"op":"screenshot","path":".."}                saves a PNG of the window
//   {"op":"quit"}                                  disconnects (the test stops the VS Code process itself)
// Nothing here evaluates a string in the page: VS Code's workbench enforces Trusted Types, which blocks `eval` and `new Function` there,
// so Playwright's string predicates (`waitForFunction("...")`) fail with "requires 'TrustedScript' assignment". Waits use locators,
// reads use function values, which Playwright sends over the protocol as they are.
// Playwright comes from the UI's lockfile; PUDDLE_VM_BROWSER_MODULES names the node_modules folder that holds `playwright-core`.
import { createRequire } from "node:module";
import { join } from "node:path";
import { createInterface } from "node:readline";

const write = (body) => process.stdout.write(`${JSON.stringify(body)}\n`);

let browser;
const logs = [];
const errors = [];

// The explorer's top-level rows and the remote indicator: what "the window is attached and shows the workspace" means.
const FOLDER_ROWS = ".explorer-folders-view .monaco-list-row .label-name";
const REMOTE = '[id="status.host"]';

// Read in the page (a function value, not a string).
const readState = () => ({
  folders: Array.from(document.querySelectorAll(".explorer-folders-view .monaco-list-row .label-name")).map((e) => e.textContent.trim()),
  remote: (document.querySelector('[id="status.host"]')?.textContent ?? "").trim(),
  title: document.title,
});

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

const attached = async (timeoutMs) => {
  const page = workbench();
  await page.locator(FOLDER_ROWS).first().waitFor({ state: "visible", timeout: timeoutMs });
  await page.locator(REMOTE).filter({ hasText: /SSH:/ }).waitFor({ state: "visible", timeout: timeoutMs });
  return page.evaluate(readState);
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

// In a multi-root workspace the new-terminal command first asks which folder the terminal starts in.
const PICKER = ".quick-input-widget:visible";
const TERMINAL = ".terminal-wrapper .xterm-helper-textarea";

const terminal = async (line, timeoutMs) => {
  const page = workbench();
  await palette("Terminal: Create New Terminal", timeoutMs);
  const asking = page.locator(PICKER).filter({ hasText: /working directory/i });
  const typing = page.locator(TERMINAL).first();
  await Promise.race([asking.waitFor({ state: "visible", timeout: timeoutMs }), typing.waitFor({ state: "attached", timeout: timeoutMs })]);
  if (await asking.isVisible()) await page.keyboard.press("Enter");
  await typing.waitFor({ state: "attached", timeout: timeoutMs });
  await typing.focus({ timeout: timeoutMs });
  await page.keyboard.type(line);
  await page.keyboard.press("Enter");
  return null;
};

// The input box an extension opened to ask the person something (`showInputBox`): its text, then Escape, which answers it
// with nothing. Null when the window asks nothing within the time.
const prompt = async (timeoutMs) => {
  const page = workbench();
  const box = page.locator(PICKER).first();
  try {
    await box.waitFor({ state: "visible", timeout: timeoutMs });
  } catch {
    return null;
  }
  const text = await box.innerText();
  await page.keyboard.press("Escape");
  await box.waitFor({ state: "hidden", timeout: timeoutMs });
  return text;
};

// Electron reloads the window in place: the page object stays, its document is replaced, and `load` fires for the new one.
const reload = async (timeoutMs) => {
  const page = workbench();
  const loaded = page.waitForEvent("load", { timeout: timeoutMs });
  await palette("Developer: Reload Window", timeoutMs);
  await loaded;
  return attached(timeoutMs);
};

const run = async (command) => {
  switch (command.op) {
    case "connect":
      return connect(command);
    case "attached":
      return attached(command.timeoutMs);
    case "reload":
      return reload(command.timeoutMs);
    case "palette":
      return palette(command.label, command.timeoutMs);
    case "terminal":
      return terminal(command.line, command.timeoutMs);
    case "prompt":
      return prompt(command.timeoutMs);
    case "eval":
      // A function made here and sent as a value, so the page never evaluates a string.
      return workbench().evaluate(new Function(`return (${command.expression});`));
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
