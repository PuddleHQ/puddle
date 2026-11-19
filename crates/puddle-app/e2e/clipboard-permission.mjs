// SPDX-License-Identifier: AGPL-3.0-or-later
// What a workspace window does when a page reads the clipboard by script, in the real shell
// (WebView2, so Windows only). The unit tests decide the answer for each setting; this shows the
// webview obeys it and that "Ask" is asked once per workspace:
//   deny  - `navigator.clipboard.readText()` rejects (NotAllowedError) and no prompt shows;
//   allow - it resolves with the text on the clipboard and no prompt shows;
//   ask   - the webview's own permission prompt shows (found with UI Automation), Allow resolves
//           the read, a new session on the same workspace reads with no prompt (the webview kept
//           the answer in the workspace's data folder), and another workspace is asked again;
//   Ctrl+V pastes into a field of the page under deny (a paste is the person's own keystroke, not a
//   script's read).
// The probe window is the one sandbox-window.mjs uses, opened with the setting the session names
// (PUDDLE_APP_SANDBOX_PROBE_CLIPBOARD). Plain Node 24, no packages.
//
// Environment: PUDDLE_APP_EXE (a debug build with all features), CDP_PORT (default 9340; each session
// uses the next port).
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { connect } from "./cdp.mjs";
import { onChildEnd } from "./child-end.mjs";
import { GiveUp, until } from "./poll.mjs";
import { Uia } from "./uia.mjs";

const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
const basePort = Number(process.env.CDP_PORT ?? 9340);
const CLIP = "puddle clipboard canary 7f3a";

// A prompt nobody answers must not hold the job to its step limit.
setTimeout(() => {
  console.error("clipboard permission test: no end after 8 minutes");
  spawnSync("taskkill", ["/IM", "puddle-app.exe", "/T", "/F"], { stdio: "ignore" });
  process.exit(1);
}, 8 * 60_000).unref();

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

const server = createServer((_req, res) => {
  res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
  res.end('<!doctype html><title>clipboard probe</title><textarea id="field" rows="3" cols="30"></textarea>');
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const sitePort = server.address().port;
const scratch = mkdtempSync(join(tmpdir(), "puddle-clipboard-permission-"));
const powershell = (script) => spawnSync("powershell", ["-NoProfile", "-Command", script], { encoding: "utf8" });
const setClipboard = (text) => powershell(`Set-Clipboard -Value '${text}'`);

// The read, started and not awaited: with "Ask" it stays pending behind the prompt.
// (The expression ends in `true`: `evaluate` awaits a promise it gets back, and this one waits for the person.)
const START_READ = `window.__read = navigator.clipboard.readText().then((text) => ({ text }), (e) => ({ error: e.name })); true`;
const readResult = (page) => page.evaluate(`Promise.race([window.__read, new Promise((r) => setTimeout(() => r({ pending: true }), 1500))])`);

/** One run of the shell on `workspace` with the clipboard setting `setting`; `body` gets the page and the UI Automation driver. */
async function session(n, workspace, setting, body) {
  const port = basePort + n;
  const env = {
    ...process.env,
    PUDDLE_APP_FIXTURE: "1",
    PUDDLE_APP_DATA_DIR: join(scratch, "data"),
    PUDDLE_APP_SANDBOX_PROBE: `http://${workspace}.localhost:${sitePort}/`,
    PUDDLE_APP_SANDBOX_DEBUG_PORT: String(port),
    PUDDLE_APP_SANDBOX_PROBE_WORKSPACE: workspace,
    PUDDLE_APP_SANDBOX_PROBE_CLIPBOARD: setting,
  };
  const app = spawn(exe, [], { stdio: ["ignore", "inherit", "inherit"], env });
  let exited = false;
  onChildEnd(app, () => {
    exited = true;
  });
  let page;
  let uia;
  try {
    const target = await until(`the window on ${workspace}.localhost`, async () => {
      if (exited) throw new GiveUp("puddle-app exited early");
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      return list.find((t) => t.type === "page" && t.url.startsWith(`http://${workspace}.localhost:${sitePort}/`));
    });
    page = await connect(target.webSocketDebuggerUrl);
    await until("the page to load", async () => (await page.evaluate(`!!document.getElementById("field")`)) === true);
    uia = await Uia.start(app.pid);
    await body({ page, uia });
  } finally {
    // Closing the window ends the browser process, which is what writes the permission down.
    await page?.send("Page.close").catch(() => undefined);
    uia?.close();
    await until("the browser process to exit", async () => {
      try {
        await fetch(`http://127.0.0.1:${port}/json/version`);
        return false;
      } catch {
        return true;
      }
    }).catch(() => undefined);
    if (!exited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
    page?.close();
  }
}

/** The prompt's buttons that allow the read, from any window of the app. */
const allowButtons = (uia) => uia.find({ role: "button", nameRegex: "^Allow" });
const promptShowing = async (uia) => (await allowButtons(uia)).length > 0;

try {
  setClipboard(CLIP);

  await session(1, "denyws", "deny", async ({ page, uia }) => {
    await page.evaluate(START_READ);
    const r = await readResult(page);
    check("deny: readText rejects with no prompt", r.error === "NotAllowedError", JSON.stringify(r));
    check("deny: no permission prompt shows", !(await promptShowing(uia)));
    // Ctrl+V is the person's own paste, not a script's read.
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    powershell("Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('^v')");
    const pasted = await until("the paste to land in the field", async () => (await page.evaluate(`document.getElementById("field").value`)) || false, 10_000).catch(() => "");
    check("deny: Ctrl+V still pastes into the page", pasted === CLIP, JSON.stringify(pasted));
  });

  await session(2, "allowws", "allow", async ({ page, uia }) => {
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    await page.evaluate(START_READ, true);
    const r = await readResult(page);
    check("allow: readText resolves with the clipboard's text", r.text === CLIP, JSON.stringify(r));
    check("allow: no permission prompt shows", !(await promptShowing(uia)));
  });

  await session(3, "askws", "ask", async ({ page, uia }) => {
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    await page.evaluate(START_READ, true);
    const first = await readResult(page);
    check("ask: the read waits for the person", first.pending === true, JSON.stringify(first));
    const buttons = await until("the permission prompt", async () => {
      const found = await allowButtons(uia);
      return found.length > 0 ? found : false;
    }, 20_000).catch(async (err) => {
      console.log(`     ${err.message}; the app's windows:\n${(await uia.dump()).join("\n")}`);
      return [];
    });
    check("ask: the webview's permission prompt shows", buttons.length > 0, JSON.stringify(buttons.map((b) => b.name)));
    if (buttons.length > 0) {
      await uia.click("the Allow button", { role: "button", name: buttons[0].name });
      const r = await until("the read to settle", async () => {
        const v = await page.evaluate(`Promise.race([window.__read, new Promise((r) => setTimeout(() => r(null), 500))])`);
        return v ?? false;
      }, 15_000).catch(() => ({ pending: true }));
      check("ask: Allow resolves the read with the clipboard's text", r.text === CLIP, JSON.stringify(r));
    }
  });

  // Same workspace, new session: the answer is in its data folder.
  await session(4, "askws", "ask", async ({ page, uia }) => {
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    await page.evaluate(START_READ, true);
    const r = await readResult(page);
    check("ask: the same workspace is not asked again", r.text === CLIP && !(await promptShowing(uia)), JSON.stringify(r));
  });

  // Another workspace has a data folder of its own and is asked on its own.
  await session(5, "otherws", "ask", async ({ page, uia }) => {
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    await page.evaluate(START_READ, true);
    const asked = await until("the prompt of another workspace", async () => (await promptShowing(uia)) || false, 20_000).catch(() => false);
    check("ask: another workspace is asked on its own", asked === true);
  });
} catch (err) {
  check("the clipboard permission test ran to the end", false, String(err?.stack ?? err));
} finally {
  server.close();
  await sleep(500);
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
console.log("\nclipboard permission test passed");
process.exit(0);
