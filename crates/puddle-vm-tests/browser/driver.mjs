// SPDX-License-Identifier: GPL-3.0-or-later
// Browser half of the VM-tier browser client (crates/puddle-vm-tests/src/browser.rs).
//
// One headless Chromium with one browser context (one cookie jar, like one window's data folder),
// driven by JSON lines on stdin; every command gets one JSON line on stdout:
//   {"op":"cookies","cookies":[{name,value,domain,path,httpOnly,sameSite}]}   adds cookies
//   {"op":"open","page":<id>,"url":...,"ready":...,"result":...,"timeoutMs":...,"waitUntil":"domcontentloaded"|"commit"}
//       opens a new page, waits until `ready` is truthy and evaluates `result`; the page stays open
//   {"op":"wait","page":<id>,"ready":...,"result":...,"timeoutMs":...}   the same on an open page
//   {"op":"press","page":<id>,"keys":"Control+Backquote"}                  presses keys in the page
//   {"op":"close","page":<id>}                                             closes the page (its sockets end)
//   {"op":"quit"}                                                          closes the browser and exits
// Replies: {"ok":true,"result":<value>,"console":[...],"pageErrors":[...]} or
//          {"ok":false,"error":"...","console":[...],"pageErrors":[...]}; console and errors are
// those of the command's page since its last reply.
// Playwright comes from the UI's lockfile (the same pin and browser build the UI jobs use);
// PUDDLE_VM_BROWSER_MODULES names the node_modules folder that holds `playwright-core`.
import { createRequire } from "node:module";
import { join } from "node:path";
import { createInterface } from "node:readline";

const pages = new Map();
const write = (body) => process.stdout.write(`${JSON.stringify(body)}\n`);

let browser;
let context;
const start = async (ignoreHttpsErrors) => {
  const modules = process.env.PUDDLE_VM_BROWSER_MODULES;
  if (!modules) throw new Error("PUDDLE_VM_BROWSER_MODULES is not set");
  const { chromium } = createRequire(join(modules, "..", "x.js"))("playwright-core");
  browser = await chromium.launch({ headless: true });
  context = await browser.newContext({ ignoreHTTPSErrors: ignoreHttpsErrors === true });
};

const page = (id) => {
  const entry = pages.get(id);
  if (!entry) throw new Error(`no open page ${id}`);
  return entry;
};

const ready = async (entry, command) => {
  await entry.page.waitForFunction(command.ready, undefined, { timeout: command.timeoutMs, polling: 100 });
  return entry.page.evaluate(command.result);
};

const run = async (command) => {
  switch (command.op) {
    case "start":
      await start(command.ignoreHttpsErrors);
      return null;
    case "cookies":
      await context.addCookies(command.cookies);
      return null;
    case "open": {
      const entry = { page: await context.newPage(), console: [], errors: [] };
      entry.page.on("console", (message) => entry.console.push(`${message.type()}: ${message.text()}`));
      entry.page.on("pageerror", (error) => entry.errors.push(String(error)));
      pages.set(command.page, entry);
      await entry.page.goto(command.url, { timeout: command.timeoutMs, waitUntil: command.waitUntil });
      return ready(entry, command);
    }
    case "wait":
      return ready(page(command.page), command);
    case "press":
      await page(command.page).page.keyboard.press(command.keys);
      return null;
    case "close": {
      const entry = page(command.page);
      pages.delete(command.page);
      await entry.page.close();
      return null;
    }
    default:
      throw new Error(`unknown command ${command.op}`);
  }
};

const lines = createInterface({ input: process.stdin });
for await (const line of lines) {
  if (!line.trim()) continue;
  let command = {};
  try {
    command = JSON.parse(line);
    if (command.op === "quit") break;
    const result = await run(command);
    const entry = pages.get(command.page);
    write({ ok: true, result, console: entry?.console.splice(0) ?? [], pageErrors: entry?.errors.splice(0) ?? [] });
  } catch (error) {
    const entry = pages.get(command.page);
    write({
      ok: false,
      error: String(error?.message ?? error),
      console: entry?.console.splice(0) ?? [],
      pageErrors: entry?.errors.splice(0) ?? [],
    });
  }
}
lines.close();
await browser?.close();
// stdin stays open after a quit, and an open stdin keeps node alive.
process.exit(0);
