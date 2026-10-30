// SPDX-License-Identifier: GPL-3.0-or-later
// Browser half of the VM-tier browser client (crates/puddle-vm-tests/src/browser.rs).
//
// Reads one JSON request from stdin, opens the page in headless Chromium, waits until the
// request's `ready` expression is truthy, evaluates `result`, and prints one JSON line:
//   {"ok":true,"result":<value>,"console":[...],"pageErrors":[...]}
//   {"ok":false,"error":"...","console":[...],"pageErrors":[...]}
// Playwright comes from the UI's lockfile (the same pin and browser build the UI jobs use);
// PUDDLE_VM_BROWSER_MODULES names the node_modules folder that holds `playwright-core` (a folder named node_modules).
import { createRequire } from "node:module";
import { join } from "node:path";

const request = JSON.parse(await new Promise((done, fail) => {
  let text = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => (text += chunk));
  process.stdin.on("end", () => done(text));
  process.stdin.on("error", fail);
}));

const consoleLines = [];
const pageErrors = [];
const reply = (body) => process.stdout.write(`${JSON.stringify({ ...body, console: consoleLines, pageErrors })}\n`);

let browser;
try {
  const modules = process.env.PUDDLE_VM_BROWSER_MODULES;
  if (!modules) throw new Error("PUDDLE_VM_BROWSER_MODULES is not set");
  const { chromium } = createRequire(join(modules, "..", "x.js"))("playwright-core");
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ ignoreHTTPSErrors: request.ignoreHttpsErrors === true });
  const page = await context.newPage();
  page.on("console", (message) => consoleLines.push(`${message.type()}: ${message.text()}`));
  page.on("pageerror", (error) => pageErrors.push(String(error)));
  await page.goto(request.url, { timeout: request.timeoutMs, waitUntil: "domcontentloaded" });
  await page.waitForFunction(request.ready, undefined, { timeout: request.timeoutMs, polling: 100 });
  reply({ ok: true, result: await page.evaluate(request.result) });
} catch (error) {
  reply({ ok: false, error: String(error?.message ?? error) });
} finally {
  await browser?.close();
}
