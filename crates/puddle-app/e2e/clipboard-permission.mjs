// SPDX-License-Identifier: AGPL-3.0-or-later
// What a workspace window does when a page reads the clipboard by script, in the real shell
// (WebView2, so Windows only). The unit tests decide the answer for each setting; this shows the
// webview obeys it, and that "Ask" waits for the person:
//   deny  - `navigator.clipboard.readText()` rejects (NotAllowedError) with nobody answering;
//   allow - it resolves with the text on the clipboard with nobody answering;
//   ask   - the read neither resolves nor rejects: it waits for the person at the webview's own prompt;
//   Ctrl+V pastes into a field of the page under deny (a paste is the person's own keystroke, not a
//   script's read).
// Not shown here: the prompt itself and that its answer is kept per workspace. On the hosted runner the
// prompt is in no window UI Automation lists (every top-level window of the desktop and everything
// under the app's processes were listed, none had it), so nothing can press Allow.
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

const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
const basePort = Number(process.env.CDP_PORT ?? 9340);
const CLIP = "Puddle clipboard canary 7f3a";

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
  try {
    const target = await until(`the window on ${workspace}.localhost`, async () => {
      if (exited) throw new GiveUp("puddle-app exited early");
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      return list.find((t) => t.type === "page" && t.url.startsWith(`http://${workspace}.localhost:${sitePort}/`));
    });
    page = await connect(target.webSocketDebuggerUrl);
    await until("the page to load", async () => (await page.evaluate(`!!document.getElementById("field")`)) === true);
    await body({ page });
  } finally {
    // Closing the window ends the browser process, which is what writes the permission down.
    await page?.send("Page.close").catch(() => undefined);
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

try {
  setClipboard(CLIP);

  await session(1, "denyws", "deny", async ({ page }) => {
    await page.evaluate(START_READ);
    const r = await readResult(page);
    check("deny: readText rejects with no prompt", r.error === "NotAllowedError", JSON.stringify(r));
    // Ctrl+V is the person's own paste, not a script's read.
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    powershell("Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait('^v')");
    const pasted = await until("the paste to land in the field", async () => (await page.evaluate(`document.getElementById("field").value`)) || false, 10_000).catch(() => "");
    check("deny: Ctrl+V still pastes into the page", pasted === CLIP, JSON.stringify(pasted));
  });

  await session(2, "allowws", "allow", async ({ page }) => {
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    await page.evaluate(START_READ, true);
    const r = await readResult(page);
    check("allow: readText resolves with the clipboard's text", r.text === CLIP, JSON.stringify(r));
  });

  await session(3, "askws", "ask", async ({ page }) => {
    await page.send("Page.bringToFront");
    await page.evaluate(`document.getElementById("field").focus(); true`);
    await page.evaluate(START_READ, true);
    const first = await readResult(page);
    check("ask: the read waits for the person", first.pending === true, JSON.stringify(first));
    await sleep(5000);
    const later = await readResult(page);
    check("ask: it is still waiting, neither allowed nor refused on its own", later.pending === true, JSON.stringify(later));
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
