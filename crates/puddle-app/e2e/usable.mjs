// SPDX-License-Identifier: GPL-3.0-or-later
// The "usable" run (install, create, approve, edit, build, push), with no person: the installed, release build of the app on
// Windows, driven over WebView2's DevTools Protocol the way a user would use it.
//
//   1. an SSH remote in the create form is refused with the explicit message;
//   2. a workspace is created from the lab Forgejo's private repository (a Forgejo built for the
//      run; its root is in the machine's certificate store, like a company root); whatever the app
//      asks the user to approve in the inbox is approved by clicking Allow;
//   3. the browser editor opens in its own window and shows the workbench;
//   4. a real `apt-get install nodejs npm` (approved in the inbox) and a real `npm run build` and
//      `npm test` run in the workspace's checkout;
//   5. `git push` from the workspace reaches Forgejo with the credential Puddle injects, the
//      author is the identity's, and the guest, the audit log and the API never hold the token.
//
// The app is the installed one (`puddle-app.exe` in PUDDLE_INSTALL_DIR). A release build has none of
// the debug hooks (PUDDLE_APP_DEBUG_PORT), so the windows are reached through WebView2's own
// environment variable, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS, with `--remote-debugging-port=0` (a
// port per browser process, each written to its DevToolsActivePort file). The checks that a person
// does by looking (copy, layout, whether the editor "feels" right) are not here. Plain Node 24, no packages.
//
// Environment: USABLE_INFO (the JSON `puddle-lab-forgejo --info` wrote), PUDDLE_INSTALL_DIR (default
// %LOCALAPPDATA%\Programs\Puddle), USABLE_OUT (a folder for screenshots and logs; default a temp one).
import { spawn, spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { connect } from "./cdp.mjs";

const local = process.env.LOCALAPPDATA;
if (!local) throw new Error("LOCALAPPDATA is not set: this runs on Windows");
const installDir = process.env.PUDDLE_INSTALL_DIR ?? join(local, "Programs", "Puddle");
const dataDir = join(local, "puddle");
const out = process.env.USABLE_OUT ?? join(tmpdir(), "puddle-usable");
mkdirSync(out, { recursive: true });
const info = JSON.parse(readFileSync(process.env.USABLE_INFO ?? (() => { throw new Error("set USABLE_INFO"); })(), "utf8"));
const forge = info.name;
const httpsUrl = `https://${forge}/${info.owner}/${info.repo}.git`;
const sshUrl = `git@${forge}:${info.owner}/${info.repo}.git`;
const WORKSPACE = "usable";

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}
function note(text) {
  console.log(`     ${text}`);
}

/** Polls `probe` until it is truthy, for `within` ms; the reason is named at each call, never a fixed sleep. */
async function until(what, probe, within = 60_000, every = 500) {
  const deadline = Date.now() + within;
  for (;;) {
    const value = await Promise.resolve(probe()).catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out after ${Math.round(within / 1000)} s waiting for ${what}`);
    await sleep(every);
  }
}

// ---- the app and its windows -------------------------------------------------------------------

/** Every WebView2 browser process of the app writes its debugging port to a DevToolsActivePort file. */
function portFiles() {
  const found = [];
  const walk = (dir, depth) => {
    let entries;
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const e of entries) {
      if (e.isFile() && e.name === "DevToolsActivePort") found.push(join(dir, e.name));
      else if (e.isDirectory() && depth > 0) walk(join(dir, e.name), depth - 1);
    }
  };
  walk(join(local, "be.tijsvankampen.puddle"), 3);
  walk(join(dataDir, "webview"), 4);
  return found;
}

/** The page targets of every browser process of the app, each with the port that serves it. */
async function pages() {
  const seen = new Set();
  const all = [];
  for (const file of portFiles()) {
    const port = Number(readFileSync(file, "utf8").split(/\r?\n/)[0]);
    if (!port || seen.has(port)) continue;
    seen.add(port);
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(3000) })).json();
      for (const t of list) if (t.type === "page") all.push({ port, ...t });
    } catch {
      // A browser process that has gone: its file is stale.
    }
  }
  return all;
}

const app = spawn(join(installDir, "puddle-app.exe"), [], {
  stdio: ["ignore", "inherit", "pipe"],
  env: { ...process.env, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: "--remote-debugging-port=0 --remote-allow-origins=*" },
});
let appLog = "";
app.stderr.on("data", (chunk) => {
  appLog += chunk;
  process.stderr.write(chunk);
});
let appExited = false;
app.on("exit", (code) => {
  appExited = true;
  console.log(`puddle-app exited (${code})`);
});

let page;
const run = (body, gesture = false) => page.evaluate(`(async () => { ${body} })()`, gesture);
let shots = 0;
async function shot(name) {
  try {
    const { data } = await page.send("Page.captureScreenshot", { format: "png" });
    writeFileSync(join(out, `${String(++shots).padStart(2, "0")}-${name}.png`), Buffer.from(data, "base64"));
  } catch (err) {
    note(`no screenshot of ${name}: ${err}`);
  }
}

/** The app's own REST API, called from the page the way the SPA does (the page holds the token). */
async function api(method, path, body) {
  const reply = await run(`const r = await fetch(${JSON.stringify(path)}, { method: ${JSON.stringify(method)},
    headers: { Authorization: "Bearer " + window.__PUDDLE__.token, "Content-Type": "application/json" },
    body: ${body === undefined ? "undefined" : JSON.stringify(JSON.stringify(body))} });
    return { status: r.status, body: await r.text() };`);
  let json;
  try {
    json = JSON.parse(reply.body);
  } catch {
    json = undefined;
  }
  return { status: reply.status, body: reply.body, json };
}

// ---- the guest, through the installed runtime's own msb -----------------------------------------

const msbEnv = {
  ...process.env,
  MSB_PATH: join(installDir, "runtime", "msb.exe"),
  MSB_HOME: join(dataDir, "msb"),
  MSB_CONFIG_PATH: join(dataDir, "msb", "config.json"),
};

/** A command in the guest, as root or as the session user `agent` (the user a terminal opens as). */
function guest(id, script, { root = false, within = 600_000 } = {}) {
  const wrapper = root
    ? 'exec sh -c "$1"'
    : 'if id agent >/dev/null 2>&1; then exec runuser -u agent -- sh -c "$1"; else exec sh -c "$1"; fi';
  const child = spawn(msbEnv.MSB_PATH, ["exec", id, "--", "sh", "-c", wrapper, "_", script], { env: msbEnv, stdio: ["ignore", "pipe", "pipe"] });
  let text = "";
  child.stdout.on("data", (c) => (text += c));
  child.stderr.on("data", (c) => (text += c));
  const timer = setTimeout(() => child.kill(), within);
  const done = new Promise((resolve) =>
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({ code, text });
    }),
  );
  return done;
}

// ---- the inbox ------------------------------------------------------------------------------------

const approved = [];

/** Clicks Allow on every pending request the inbox shows, as the user does, until `done` settles. */
async function approveUntil(what, done) {
  let finished = false;
  const result = done.then((value) => {
    finished = true;
    return value;
  });
  await run(`document.querySelector('nav[aria-label="Main"] a[href="/inbox"]')?.click(); return true;`);
  while (!finished) {
    const clicked = await run(`const hits = [...document.querySelectorAll('button[aria-label^="Allow "]')]
        .filter((b) => /^Allow \\S+ for /.test(b.getAttribute("aria-label")));
      const labels = hits.map((b) => b.getAttribute("aria-label"));
      hits.forEach((b) => b.click());
      return labels;`).catch(() => []);
    for (const label of clicked) {
      approved.push(label);
      note(`approved in the inbox (${what}): ${label}`);
    }
    await Promise.race([result, sleep(clicked.length > 0 ? 500 : 1000)]);
  }
  return result;
}

// ---- the run ---------------------------------------------------------------------------------------

try {
  // The app is up when one of its windows lists a page served from 127.0.0.1.
  const main = await until(
    "the main window's page in WebView2's targets (is the debugging port reaching a release build?)",
    async () => (await pages()).find((t) => /^http:\/\/127\.0\.0\.1:\d+\//.test(t.url)),
    180_000,
    1000,
  );
  page = await connect(main.webSocketDebuggerUrl);
  note(`main window: ${main.url} on debugging port ${main.port}`);

  // First run: skip the setup, as a user may.
  await until("the first-run screen", () => run(`return [...document.querySelectorAll('button')].some((b) => b.textContent.trim() === "Skip setup");`), 90_000);
  await run(`[...document.querySelectorAll('button')].find((b) => b.textContent.trim() === "Skip setup")?.click(); return true;`);
  await until("the sidebar", () => run(`return document.querySelectorAll('nav[aria-label="Main"] a').length >= 4;`), 60_000);
  check("the installed app opens and shows its sidebar", true);

  // What the settings screen and the identity forms do through the same API; the forge is on this
  // computer, so the workspace may reach this computer's own addresses (the Loopback switch).
  const toggled = await api("PUT", "/api/settings", { workspace_defaults: { local_toggles: { loopback: true } } });
  check("the 'this computer' switch is on for new workspaces", toggled.status === 200, `${toggled.status} ${toggled.body.slice(0, 200)}`);
  const stored = await api("POST", "/api/credentials/stored", { host: forge, token: info.token });
  check("the access token is pasted and kept", stored.status === 201 && !stored.body.includes(info.token), `${stored.status}`);
  const identity = await api("POST", "/api/identities", {
    label: "Lab",
    author: { name: "Usable Run", email: "usable@example.org" },
    credentials: [{ host: forge, source: stored.json.source, covers: { owners: [info.owner], rest_of_host: false } }],
  });
  check("an identity covers the forge's account", identity.status === 201, `${identity.status} ${identity.body.slice(0, 200)}`);

  // 1. The create form: an SSH remote is refused with the explicit message.
  const nav = (href) => run(`document.querySelector('nav[aria-label="Main"] a[href="${href}"]')?.click(); return location.pathname;`);
  await nav("/workspaces");
  await until("the Workspaces screen", () => run(`return location.pathname === "/workspaces" && [...document.querySelectorAll("button")].some((b) => /New workspace/.test(b.textContent));`));
  const fill = (selector, value) => run(`const el = document.querySelector(${JSON.stringify(selector)}); if (!el) throw new Error("no ${selector}");
    el.focus(); el.value = ${JSON.stringify(value)}; el.dispatchEvent(new Event("input", { bubbles: true })); return true;`);
  const clickByText = (selector, pattern) => run(`const b = [...document.querySelectorAll(${JSON.stringify(selector)})].find((e) => new RegExp(${JSON.stringify(pattern)}).test((e.getAttribute("aria-label") ?? e.textContent).replace(/\\s+/g, " ").trim()));
    if (!b) throw new Error("no ${selector} like ${pattern}"); b.click(); return true;`, true);
  await clickByText("button", "^New workspace");
  await until("the New workspace dialog", () => run(`return !!document.querySelector('[role="dialog"] #new-ws-name');`));
  await clickByText('[role="dialog"] button', "^Add (another|a) repository$");
  const urlInput = `[role="dialog"] fieldset input[id$="url"]`;
  await until("the repository row", () => run(`return !!document.querySelector(${JSON.stringify(urlInput)});`));
  await fill(urlInput, sshUrl);
  await fill("#new-ws-name", "ssh-refused");
  await clickByText('[role="dialog"] button[type="submit"]', "^Create workspace");
  const refusal = await until("the SSH refusal", () =>
    run(`const t = [...document.querySelectorAll('[role="dialog"] [role="alert"]')].map((e) => e.textContent.trim()).join(" | "); return t || false;`),
  );
  check("an SSH remote is refused with the explicit message", /SSH remotes are not supported yet/.test(refusal), refusal);
  const noneMade = await api("GET", "/api/workspaces");
  check("the refused create made no workspace", noneMade.json?.workspaces?.length === 0, noneMade.body.slice(0, 200));
  await shot("ssh-refused");

  // 2. The real create, from the lab Forgejo's private repository. The same form, the HTTPS address.
  await fill(urlInput, httpsUrl);
  await fill("#new-ws-name", WORKSPACE);
  await clickByText('[role="dialog"] button[type="submit"]', "^Create workspace");
  const findWorkspace = async () => (await api("GET", "/api/workspaces")).json?.workspaces?.find((w) => w.name === WORKSPACE);
  const made = await until("the workspace to exist", findWorkspace, 60_000);
  const id = made.id;
  note(`workspace ${WORKSPACE} has the id ${id}`);
  let started = false;
  const running = await approveUntil(
    "create",
    until(
      `workspace ${id} to run (the create clones the private repository, then the image is pulled and the machine boots)`,
      async () => {
        const w = (await api("GET", "/api/workspaces")).json?.workspaces?.find((x) => x.id === id);
        if (!w) throw new Error("the workspace vanished");
        if (["crashed", "volume_missing"].includes(w.status)) throw new Error(`the workspace is ${w.status}`);
        if (w.status === "running" && !w.busy) return w;
        if (!started && !w.busy && ["created", "stopped"].includes(w.status)) {
          started = true;
          await api("POST", `/api/workspaces/${id}/start`);
        }
        return false;
      },
      900_000,
      2000,
    ),
  );
  check("a workspace made from the lab Forgejo's private repository is running", running.status === "running", `${running.status}, image ${running.image}`);
  const full = (await api("GET", `/api/workspaces/${id}`)).json;
  check("the create cloned the repository", (full?.repositories ?? []).some((r) => r.url === httpsUrl), JSON.stringify(full?.repositories));
  await shot("workspace-running");

  // 3. The browser editor opens in its own window.
  await nav("/workspaces");
  await until("the workspace card", () => run(`return !![...document.querySelectorAll("article")].find((a) => a.textContent.includes(${JSON.stringify(WORKSPACE)}));`));
  await run(`const card = [...document.querySelectorAll("article")].find((a) => a.textContent.includes(${JSON.stringify(WORKSPACE)}));
    [...card.querySelectorAll("button")].find((b) => /^Connect/.test(b.textContent.trim())).click(); return true;`, true);
  await until("the Connect dialog", () => run(`return !![...document.querySelectorAll('[role="dialog"] button')].find((b) => /Open in the browser/.test(b.textContent));`));
  const before = new Set((await pages()).map((t) => t.id));
  await clickByText('[role="dialog"] button', "Open in the browser");
  const editor = await until(
    "an editor window on the workspace's own origin (the editor server is installed in the guest the first time)",
    async () => (await pages()).find((t) => !before.has(t.id) && /localhost/.test(t.url) && !/^http:\/\/127\.0\.0\.1/.test(t.url)),
    420_000,
    2000,
  );
  note(`editor window: ${editor.url.replace(/\?.*$/, "")}`);
  const win = await connect(editor.webSocketDebuggerUrl);
  const bench = await until("the editor's workbench", () => win.evaluate(`!!document.querySelector(".monaco-workbench")`), 180_000, 2000).catch(() => false);
  const winTitle = await win.evaluate("document.title").catch(() => "");
  check("the browser editor opens in its own window and shows the workbench", bench === true, `${winTitle} (${editor.url.split("/")[2]})`);
  const guarded = await win.evaluate(`(() => { const t = window.__TAURI_INTERNALS__; return !t || typeof t.invoke !== "function"; })()`).catch(() => undefined);
  note(`editor window without a Tauri bridge: ${guarded}`);
  win.close();
  await shot("after-editor-open");

  // 4. A real build. Node and npm come from the distribution's own archive (an apt mirror the user approves).
  const apt = guest(id, "export DEBIAN_FRONTEND=noninteractive; apt-get update -qq && apt-get install -y -qq --no-install-recommends nodejs npm >/tmp/apt.log 2>&1; echo apt=$?; tail -3 /tmp/apt.log", { root: true, within: 900_000 });
  const aptDone = await approveUntil("apt", apt);
  check("nodejs and npm install from the distribution's archive after the approvals", /apt=0/.test(aptDone.text), aptDone.text.slice(-300));
  const rootDir = (await guest(id, "find /workspaces -maxdepth 3 -name package.json -not -path '*/node_modules/*' | head -1 | xargs dirname")).text.trim().split("\n").pop();
  check("the checkout is in the workspace", rootDir.startsWith("/workspaces/"), rootDir);
  const build = await guest(id, `cd ${rootDir} && node --version && npm --version && npm install --no-audit --no-fund 2>&1 | tail -2 && npm run build 2>&1 && npm test 2>&1 | tail -8 && node dist/app.js`);
  check("npm run build and npm test pass in the workspace", build.code === 0 && /build ok/.test(build.text) && /hello from usable/.test(build.text), build.text.slice(-500));

  // 5. Push with the injected credential.
  const branch = `usable-${Date.now()}`;
  const push = await guest(
    id,
    `set -eu; cd ${rootDir}; git checkout -q -b ${branch}; git add -f dist/build-stamp.txt; git commit -q -m "build from the usable run"; git push -q origin ${branch} 2>&1; git log -1 --format=%ae; git rev-parse HEAD`,
  );
  const lines = push.text.trim().split("\n");
  const [author, head] = [lines.at(-2), lines.at(-1)];
  check("git push from the workspace succeeds", push.code === 0 && /^[0-9a-f]{40}$/.test(head ?? ""), push.text.slice(-400));
  check("the commit carries the identity's author", author === "usable@example.org", author);
  const onForge = await fetch(`https://${forge}/api/v1/repos/${info.owner}/${info.repo}/branches/${branch}`, { headers: { Authorization: `token ${info.token}` } });
  const forgeHead = onForge.ok ? (await onForge.json()).commit?.id : undefined;
  check("the push reached Forgejo at the same commit", forgeHead === head, `${onForge.status} ${forgeHead}`);

  // The token is nowhere a guest, the API or the audit log could show it. The control comes first: a
  // token planted in a file and in a process's environment must be found, or the search proves nothing.
  const [first, second] = [info.token.slice(0, 20), info.token.slice(20)];
  const search = `w="${first}""${second}"; printf '%s\\n' "$w" "$(printf 'x-access-token:%s' "$w" | base64 -w0)" "$(printf '%s' "$w" | base64 -w0)" > /dev/shm/.p; \
echo procs=$(cat /proc/[0-9]*/environ /proc/[0-9]*/cmdline 2>/dev/null | tr '\\0' '\\n' | grep -cFf /dev/shm/.p); \
echo files=$(grep -rlaFf /dev/shm/.p --exclude-dir=proc --exclude-dir=sys --exclude-dir=dev --exclude=.p / 2>/dev/null | grep -v '^/dev/shm/.p$' | tr '\\n' ' '); rm -f /dev/shm/.p; echo done`;
  const planted = await guest(id, `w="${first}""${second}"; printf '%s\\n' "$w" > /root/planted.txt; env PLANTED="$w" sleep 120 & pid=$!; sleep 1; ${search}; kill $pid; rm -f /root/planted.txt`, { root: true });
  check("the token search finds a planted token (the control)", !/procs=0/.test(planted.text) && planted.text.includes("/root/planted.txt"), planted.text.replace(info.token, "<token>").slice(-300));
  const found = await guest(id, search, { root: true });
  check("the guest holds no token in a process or a file", /procs=0/.test(found.text) && /files=\s*\n?done/.test(found.text), found.text.slice(-300));
  const audit = await api("GET", `/api/audit?workspace=${id}&type=connection&limit=200`);
  check("the audit log holds no token and says the connections were injected", audit.status === 200 && !audit.body.includes(info.token) && audit.body.includes('"injected":true'), `${audit.status}`);
  const listing = await api("GET", "/api/identities");
  check("the API never gives the token back", !listing.body.includes(info.token) && !stored.body.includes(info.token));
  check("the app's log holds no token", !appLog.includes(info.token));

  // The approvals were clicks in the inbox.
  check("requests were approved by clicking in the inbox", approved.length > 0, approved.join("; "));

  // Delete, as the user does at the end; no runtime process stays behind.
  const seen = await api("GET", `/api/workspaces/${id}/delete-check`);
  const gone = await api("DELETE", `/api/workspaces/${id}`, { confirm: true, fingerprint: seen.json?.fingerprint });
  check("the workspace is deleted", gone.status === 200 || gone.status === 202 || gone.status === 204, `${gone.status} ${gone.body.slice(0, 200)}`);
  await until("the workspace to be gone", async () => !(await findWorkspace()), 180_000, 2000).catch((err) => check("the workspace goes away", false, String(err)));
} catch (err) {
  check("the usable run ran to the end", false, String(err?.stack ?? err));
  await shot("failure").catch(() => {});
  try {
    const targets = (await pages()).map((t) => `${t.port} ${t.url.replace(/\?.*$/, "")}`);
    note(`windows at the end: ${targets.join(" | ") || "none"}`);
  } catch {
    // Diagnostics only.
  }
} finally {
  writeFileSync(join(out, "app.log"), appLog);
  page?.close();
  if (!appExited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\nthe usable run passed");
process.exit(0);
