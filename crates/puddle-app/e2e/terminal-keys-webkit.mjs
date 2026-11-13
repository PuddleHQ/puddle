// SPDX-License-Identifier: GPL-3.0-or-later
// The WebKitGTK half of the terminal's key assumptions, the
// engine behind the Linux shell, which has no DevTools Protocol: drives `terminal-keys.html` in
// WebKitGTK's own browser through WebKitWebDriver (W3C WebDriver over HTTP, plain Node 24, no
// packages), under a virtual display. WebKitGTK has no switch for the webview's shortcuts, so the page's
// keydown `preventDefault` is the only thing that can stop Ctrl+R and F5 here.
//   1. Ctrl+Shift+V fires `paste` with the clipboard's text. Shift+Insert does not: WebKitGTK has no
//      paste binding for it, even in a plain textarea (measured on 2.54), and the page cannot ask for
//      the paste itself (`execCommand("paste")` blocks the page on the browser's paste prompt). The
//      test pins that, so a WebKitGTK that gains the binding shows up as a failure to update the design.
//   2. Ctrl+R and F5 with the keydown cancelled do not reload the document.
//   3. Every key reached the page (so 2 is not vacuous): the page logged each keydown.
// The clipboard is filled the way a user fills it, by copying a selection in the page.
//
// Environment: WEBKIT_DRIVER (default `WebKitWebDriver`), WEBKIT_BROWSER (default
// MiniBrowser from WebKitGTK's libexec folder), DRIVER_PORT (default 4455). Run under xvfb-run.
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import { createServer } from "node:http";
import { setTimeout as sleep } from "node:timers/promises";

const driverBin = process.env.WEBKIT_DRIVER ?? "WebKitWebDriver";
const port = Number(process.env.DRIVER_PORT ?? 4455);
const browser =
  process.env.WEBKIT_BROWSER ??
  ["/usr/lib/x86_64-linux-gnu/webkit2gtk-4.1/MiniBrowser", "/usr/lib/aarch64-linux-gnu/webkit2gtk-4.1/MiniBrowser", "/usr/libexec/webkit2gtk-4.1/MiniBrowser"].find(existsSync);
if (!browser) throw new Error("no MiniBrowser found: set WEBKIT_BROWSER");

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

const page = await readFile(new URL("./terminal-keys.html", import.meta.url));
const server = createServer((_req, res) => {
  res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
  res.end(page);
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const url = `http://127.0.0.1:${server.address().port}/`;

const driver = spawn(driverBin, [`--port=${port}`], { stdio: ["ignore", "inherit", "inherit"] });
let session;
const call = async (method, path, body) => {
  const res = await fetch(`http://127.0.0.1:${port}${path}`, {
    method,
    headers: { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(30_000),
  });
  const json = await res.json();
  if (!res.ok) throw new Error(`${method} ${path}: ${JSON.stringify(json.value)}`);
  return json.value;
};
const script = (source) => call("POST", `/session/${session}/execute/sync`, { script: source, args: [] });

const KEY = { ctrl: "", shift: "", insert: "", f5: "" };
/** Presses `keys` together (modifiers first), then releases them in reverse order. */
const chord = (...keys) =>
  call("POST", `/session/${session}/actions`, {
    actions: [
      {
        type: "key",
        id: "keyboard",
        actions: [...keys.map((value) => ({ type: "keyDown", value })), ...[...keys].reverse().map((value) => ({ type: "keyUp", value }))],
      },
    ],
  });

/**
 * Copies `text` to the system clipboard as a user does: select it in a scratch field and press Ctrl+C.
 * The field's own `copy` event proves the keystroke landed in it: under Xvfb the window can still be
 * taking focus, and a Ctrl+C that reaches the probe's input is cancelled by the page and copies nothing.
 */
const copy = async (text) => {
  for (let attempt = 0; attempt < 10; attempt++) {
    await script(`
      document.getElementById("scratch")?.remove();
      const field = document.createElement("textarea");
      field.id = "scratch";
      field.value = ${JSON.stringify(text)};
      window.copied = false;
      field.addEventListener("copy", () => { window.copied = true; });
      document.body.append(field);
      field.focus();
      field.select();`);
    await chord(KEY.ctrl, "c");
    const landed = await until("the copy event", () => script("return window.copied"), 1_000).catch(() => false);
    await script(`document.getElementById("scratch").remove(); document.getElementById("input").focus();`);
    if (landed) return;
    await sleep(500);
  }
  throw new Error("Ctrl+C never reached the scratch field, so nothing was copied");
};

async function until(what, probe, within = 15_000) {
  const deadline = Date.now() + within;
  for (;;) {
    const value = await probe().catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(100);
  }
}

try {
  await until("the driver", async () => (await fetch(`http://127.0.0.1:${port}/status`)).ok);
  const made = await call("POST", "/session", {
    capabilities: { alwaysMatch: { browserName: "MiniBrowser", "webkitgtk:browserOptions": { binary: browser, args: ["--automation"] } } },
  });
  session = made.sessionId;
  await call("POST", `/session/${session}/url`, { url });
  await until("the page", () => script("return document.readyState === 'complete' && document.activeElement === document.getElementById('input')"));
  const documentId = await script("return window.probe.document");

  const text = "echo pasted from the clipboard";
  await copy(text);
  await chord(KEY.ctrl, KEY.shift, "v");
  await until("a paste from Ctrl+Shift+V", async () => (await script("return window.probe.pastes.length")) >= 1).catch(() => undefined);
  const first = await script("return window.probe.pastes");
  check("Ctrl+Shift+V fires paste with the clipboard's text", first[0] === text, JSON.stringify(first));
  await chord(KEY.shift, KEY.insert);
  await sleep(1000);
  const second = await script("return window.probe.pastes");
  check("Shift+Insert fires no paste in WebKitGTK (known gap in the design)", second.length === 1, JSON.stringify(second));

  await chord(KEY.ctrl, "r");
  await chord(KEY.f5);
  const keys = await script("return window.probe.keys");
  const saw = (pred) => keys.some(pred);
  check("Ctrl+R reached the page", saw((k) => k.ctrl && !k.shift && k.key.toLowerCase() === "r"), JSON.stringify(keys));
  check("F5 reached the page", saw((k) => k.key === "F5"), JSON.stringify(keys));
  // A reload would be under way within a moment of the key; give it two seconds, then read the
  // document's own id.
  await sleep(2000);
  const after = await script("return window.probe.document");
  check("Ctrl+R and F5 with the keydown cancelled leave the document alone", after === documentId, `${documentId} -> ${after}`);
} catch (err) {
  check("the test ran", false, String(err));
} finally {
  if (session) await call("DELETE", `/session/${session}`).catch(() => undefined);
  driver.kill();
  server.close();
}
if (failures.length > 0) {
  console.log(`${failures.length} failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("all passed");
