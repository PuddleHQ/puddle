// SPDX-License-Identifier: GPL-3.0-or-later
// Does a real browser accept an HTTPS certificate? Serves one certificate on a loopback port and opens it
// in headless Edge, Chrome and Firefox, each with a throwaway profile.
//
//   node ci/browser-cert.mjs <ok|warning> <cert.pem> <key.pem> <profile-root>
//
// Prints one line per browser, `RESULT <browser> ok` (the page loaded) or `RESULT <browser> warning <what the browser
// said>`, then `SUMMARY <n> browsers, <m> as expected`. Exit 0 only when all three answered and every answer is
// the one asked for; a browser that is missing or does not start is `RESULT <browser> error ...` and a failure,
// never a skip. Chromium browsers: DevTools Protocol, `Page.navigate`'s errorText (net::ERR_CERT_*). Firefox:
// WebDriver BiDi, the document the tab ends on (about:certerror is the warning page). Each browser reads the
// Windows certificate store the way it does for a person. Paths come from PUDDLE_EDGE, PUDDLE_CHROME and
// PUDDLE_FIREFOX when set.
import { spawn, spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createServer } from "node:https";
import { join } from "node:path";

const [expect, certFile, keyFile, profileRoot] = process.argv.slice(2);
if (!["ok", "warning"].includes(expect) || !certFile || !keyFile || !profileRoot) {
  console.error("usage: node ci/browser-cert.mjs <ok|warning> <cert.pem> <key.pem> <profile-root>");
  process.exit(64);
}
const PROGRAM_FILES = process.env.ProgramFiles ?? "C:\\Program Files";
const PROGRAM_FILES_X86 = process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)";
const BROWSERS = [
  { name: "edge", kind: "chromium", port: 9441, exe: process.env.PUDDLE_EDGE ?? join(PROGRAM_FILES_X86, "Microsoft", "Edge", "Application", "msedge.exe") },
  { name: "chrome", kind: "chromium", port: 9443, exe: process.env.PUDDLE_CHROME ?? join(PROGRAM_FILES, "Google", "Chrome", "Application", "chrome.exe") },
  { name: "firefox", kind: "firefox", port: 9442, exe: process.env.PUDDLE_FIREFOX ?? join(PROGRAM_FILES, "Mozilla Firefox", "firefox.exe") },
];

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const ws = (u) =>
  new Promise((res, rej) => {
    const s = new WebSocket(u);
    s.onopen = () => res(s);
    s.onerror = () => rej(new Error(`websocket ${u} failed`));
  });
const rpc = (s) => {
  let next = 1;
  return (method, params = {}) =>
    new Promise((res) => {
      const id = next++;
      const onMessage = (e) => {
        const m = JSON.parse(e.data);
        if (m.id !== id) return;
        s.removeEventListener("message", onMessage);
        res(m);
      };
      s.addEventListener("message", onMessage);
      s.send(JSON.stringify({ id, method, params }));
    });
};
// Polls for something that is not there yet (a browser opening its debugging port); a bound, not a pause before a result.
async function until(fn, what, tries = 200) {
  for (let i = 0; i < tries; i++) {
    try {
      const v = await fn();
      if (v) return v;
    } catch {
      /* not up yet */
    }
    await sleep(300);
  }
  throw new Error(`timeout waiting for ${what}`);
}

async function chromium(b, url, profile) {
  const child = spawn(b.exe, [`--remote-debugging-port=${b.port}`, `--user-data-dir=${profile}`, "--headless=new", "--no-first-run",
    "--disable-extensions", "--no-default-browser-check", "about:blank"], { stdio: "ignore" });
  try {
    const target = await until(async () => (await (await fetch(`http://127.0.0.1:${b.port}/json/list`)).json()).find((t) => t.type === "page"), `${b.name} page`);
    const send = rpc(await ws(target.webSocketDebuggerUrl));
    await send("Page.enable");
    const r = await send("Page.navigate", { url });
    return r.result?.errorText ? `warning ${r.result.errorText}` : "ok";
  } finally {
    kill(child);
  }
}

async function firefox(b, url, profile) {
  const child = spawn(b.exe, ["-headless", "-no-remote", "-profile", profile, `--remote-debugging-port=${b.port}`, "about:blank"], { stdio: "ignore" });
  try {
    const send = rpc(await until(() => ws(`ws://127.0.0.1:${b.port}/session`), "Firefox BiDi"));
    const sess = await send("session.new", { capabilities: {} });
    if (sess.error) throw new Error(`session.new: ${JSON.stringify(sess)}`);
    const tree = await send("browsingContext.getTree", {});
    const context = tree.result.contexts[0].context;
    const nav = await send("browsingContext.navigate", { context, url, wait: "complete" });
    const doc = await send("script.evaluate", { expression: "document.documentURI", target: { context }, awaitPromise: false });
    const at = doc.result?.result?.value ?? "";
    if (/^about:(certerror|neterror)/.test(at)) {
      const reason = /[?&]e=([^&]+)/.exec(at)?.[1] ?? "";
      return `warning ${decodeURIComponent(reason) || at.slice(0, 60)}`;
    }
    if (nav.error) return `warning ${nav.error}: ${nav.message ?? ""} (at ${at.slice(0, 60)})`;
    return "ok";
  } finally {
    kill(child);
  }
}

function kill(child) {
  if (child?.pid) spawnSync("taskkill", ["/T", "/F", "/PID", String(child.pid)], { stdio: "ignore" });
}

const server = createServer({ cert: readFileSync(certFile), key: readFileSync(keyFile) }, (_req, res) => {
  res.writeHead(200, { "content-type": "text/plain" });
  res.end("Puddle development certificate check\n");
});
await new Promise((res) => server.listen(0, "127.0.0.1", res));
const url = `https://localhost:${server.address().port}/`;
console.log(`serving ${url}, expecting ${expect}`);

let matched = 0;
for (const b of BROWSERS) {
  let result;
  try {
    result = await (b.kind === "firefox" ? firefox : chromium)(b, url, join(profileRoot, b.name));
  } catch (e) {
    result = `error ${e.message}`;
  }
  console.log(`RESULT ${b.name} ${result}`);
  if (result === expect || result.startsWith(`${expect} `)) matched++;
}
console.log(`SUMMARY ${BROWSERS.length} browsers, ${matched} as expected`);
server.close();
process.exit(matched === BROWSERS.length ? 0 : 1);
