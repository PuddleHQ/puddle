// SPDX-License-Identifier: AGPL-3.0-or-later
// VS Code half of the desktop VS Code client (crates/puddle-vm-tests/src/vscode.rs).
//
// Drives a desktop VS Code that the test started with `--remote-debugging-port`, over Electron's DevTools Protocol, with
// Playwright's `connectOverCDP`. JSON lines on stdin; every command gets one JSON line on stdout, in the shape of the browser
// client (`browser/driver.mjs`): {"ok":true,"result":...,"console":[],"pageErrors":[]} or {"ok":false,"error":"..."}.
//   {"op":"connect","port":9230,"timeoutMs":..}   connects to the window; retries a refused connection until the budget ends
//   {"op":"attached","timeoutMs":..}               waits until the workbench shows a folder in the explorer and the remote indicator
//                                                  says `SSH:`, then returns {folders, remote, title}
//   {"op":"reload","timeoutMs":..,"picture":".."}  Developer: Reload Window, waits for a new document, then as `attached` does; `picture` is saved on failure
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

// What the window shows when the palette's row cannot be clicked: whether the picker is still there, what holds the focus, and
// what the window says. The picker closes when something else takes the focus or asks a question in its place.
const describePalette = () => {
  const widget = document.querySelector(".quick-input-widget");
  const active = document.activeElement;
  const text = (selector) => [...document.querySelectorAll(selector)].map((e) => e.innerText).join(" | ").slice(0, 600);
  return {
    pickerDisplay: widget ? getComputedStyle(widget).display : "(none)",
    pickerText: widget ? widget.innerText.slice(0, 600) : "",
    focus: active ? `${active.tagName}.${active.className}`.slice(0, 200) : "(none)",
    notifications: text(".notifications-toasts, .notification-list-item"),
    dialogs: text(".monaco-dialog-box"),
    body: (document.body?.innerText ?? "").slice(0, 600),
  };
};

const CLICK_MS = 15_000;

const palette = async (label, timeoutMs, picture) => {
  const page = workbench();
  await page.keyboard.press("F1");
  await page.locator(".quick-input-widget input").waitFor({ state: "visible", timeout: timeoutMs });
  await page.keyboard.insertText(label);
  const row = page.locator(".quick-input-list .monaco-list-row .label-name").filter({ hasText: exact(label) }).first();
  await row.waitFor({ state: "visible", timeout: timeoutMs });
  try {
    // The row is on screen: a click that does not land within seconds means the picker went away, not that it is slow.
    await row.click({ timeout: CLICK_MS });
  } catch (error) {
    const seen = await page.evaluate(describePalette).catch((e) => `(unreadable: ${e.message})`);
    if (picture) await page.screenshot({ path: picture }).catch(() => {});
    throw new Error(`the palette row "${label}" could not be clicked: ${error.message.split("\n")[0]}; the window shows ${JSON.stringify(seen)}`);
  }
  return null;
};

// In a multi-root workspace the new-terminal command first asks which folder the terminal starts in. The question is the
// placeholder of the picker's input, which is no text of the widget, so a text filter never finds it.
const PICKER = ".quick-input-widget:visible";
const ASKING_FOLDER = `${PICKER} input[placeholder*="working directory" i]`;
const TERMINAL = ".terminal-wrapper .xterm-helper-textarea";

const terminal = async (line, timeoutMs) => {
  const page = workbench();
  await palette("Terminal: Create New Terminal", timeoutMs);
  const asking = page.locator(ASKING_FOLDER);
  const typing = page.locator(TERMINAL).first();
  // Whichever comes first; it fails only when neither came.
  await Promise.any([asking.waitFor({ state: "visible", timeout: timeoutMs }), typing.waitFor({ state: "attached", timeout: timeoutMs })]);
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

// Electron replaces the window's document in place. The `load` event is not a reliable witness for that (a run on the hosted
// runner never saw it for a reload that happened), so the old document carries a marker the new one cannot have; the wait ends
// when the marker is gone, and `waitForFunction` survives the navigation. A failure says what the window showed, with a picture.
const reload = async (timeoutMs, picture) => {
  await workbench().evaluate(() => {
    window.__puddleOldDocument = true;
  });
  await palette("Developer: Reload Window", timeoutMs, picture);
  try {
    await workbench().waitForFunction(() => window.__puddleOldDocument !== true, null, { timeout: timeoutMs });
  } catch (error) {
    const text = await workbench()
      .evaluate(() => (document.body?.innerText ?? "").slice(0, 2000))
      .catch((e) => `(unreadable: ${e.message})`);
    if (picture) await workbench().screenshot({ path: picture }).catch(() => {});
    throw new Error(`the window did not replace its document: ${error.message}; it shows: ${text}`);
  }
  return attached(timeoutMs);
};

const run = async (command) => {
  switch (command.op) {
    case "connect":
      return connect(command);
    case "attached":
      return attached(command.timeoutMs);
    case "reload":
      return reload(command.timeoutMs, command.picture);
    case "palette":
      return palette(command.label, command.timeoutMs, command.picture);
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
