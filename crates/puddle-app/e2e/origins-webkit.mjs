// SPDX-License-Identifier: AGPL-3.0-or-later
// What the real Linux webview (WebKitGTK) keeps for a workspace's origins, the Linux half of
// origins.mjs: local storage, IndexedDB and a cookie on `<id>.localhost` and on a forwarded port's
// `<port>.<id>.localhost`, kept under the workspace's own data folder. Plain Node 24, no packages,
// under a virtual display (xvfb-run).
//
// WebKitGTK has no DevTools Protocol, so the shell is driven through WebKitWebDriver (W3C WebDriver
// over HTTP) with `puddle-app` itself as the browser, the way tauri-driver does it:
// `TAURI_WEBVIEW_AUTOMATION=true` makes Tauri allow automation, and WebKitGTK automates ONE web
// context, the first window's. The shell opens the probe window first under that variable, so
// the driver sees the workspace's window. A window on a forwarded port has a data folder, so a web
// context, of its own: the driver cannot see it. The test opens it the way a user does (clicking a
// button in the workspace page, the only gesture WebKitGTK lets open a window), and the page it loads
// reads and writes its own storage and reports to this test's server.
//
// The sessions are those of origins.mjs, five runs of the shell over one data folder:
//   1. create: `<id>.localhost` and two forwarded ports start empty, take a write each, and read
//      back only their own (the forwarded ports share one data folder but not an origin).
//   2. rename: a rename changes the display name and never the id, the origin or the data folder,
//      so a new session on the same id and folder finds it all.
//   3. recreate: a workspace made later has another id, so another origin and empty storage.
//   4. delete: the first id's data is removed (the shell logs it); the same id is then empty.
//   5. the second workspace's data is untouched by the delete.
// A session ends by closing the workspace's window and stopping the shell, then waiting for the
// shell to exit. Local storage is the one store WebKit writes out late (about half a second after a
// write, not at once like IndexedDB and cookies), and a session lasts about as long, so a session
// first waits until every value it wrote is in its local storage files.
//
// Environment: PUDDLE_APP_EXE (a debug build with all features: the in-memory fixture backend),
// WEBKIT_DRIVER (default `WebKitWebDriver`), DRIVER_PORT (default 4466).
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, mkdtempSync, readdirSync, readFileSync, readlinkSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { onChildEnd } from "./child-end.mjs";
import { GiveUp, until } from "./poll.mjs";

const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
const driverBin = process.env.WEBKIT_DRIVER ?? "WebKitWebDriver";
const driverPort = Number(process.env.DRIVER_PORT ?? 4466);
const FORWARDED = [5173, 5174];

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

// The same reads and writes run in the workspace's window (through the driver) and in a forwarded
// window (its own page script): these two functions only touch browser globals and are sent as source.
async function writeProbe(tag) {
  localStorage.setItem("probe", tag);
  await new Promise((resolve, reject) => {
    const open = indexedDB.open("probe", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("kv");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const tx = open.result.transaction("kv", "readwrite");
      tx.objectStore("kv").put(tag, "probe");
      tx.oncomplete = () => {
        open.result.close();
        resolve();
      };
      tx.onerror = () => reject(tx.error);
    };
  });
  document.cookie = `probe=${tag}; path=/; max-age=3600`;
  return location.origin;
}

async function readProbe() {
  const idb = await new Promise((resolve, reject) => {
    const open = indexedDB.open("probe", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("kv");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const get = open.result.transaction("kv").objectStore("kv").get("probe");
      get.onsuccess = () => {
        open.result.close();
        resolve(get.result ?? null);
      };
      get.onerror = () => reject(get.error);
    };
  });
  const cookie = document.cookie.split("; ").find((c) => c.startsWith("probe="));
  return { origin: location.origin, local: localStorage.getItem("probe"), idb, cookie: cookie ? cookie.slice(6) : null };
}

const hostsOf = (id) => ({ workspace: `${id}.localhost`, ...Object.fromEntries(FORWARDED.map((p) => [p, `${p}.${id}.localhost`])) });
const tagOf = (id, which) => `${which}-of-${id}`;

// The tag a forwarded page is to write for its host (null: only read), and what the pages reported.
let tagFor = (_host) => null;
const reports = new Map();

// The workspace's page: one button per forwarded port that opens its `/proxy/<port>/` window, as the
// editor's links do. A click is the one gesture WebKitGTK lets open a window (a script's own
// `window.open` and a `target="_blank"` link are not honoured under WebDriver).
const workspacePage = `<!doctype html><title>workspace</title>${FORWARDED.map((p) => `<button id="p${p}" onclick="window.open('/proxy/${p}/')">${p}</button> `).join("")}`;
// A forwarded page reads its storage, writes when told to, reads again and reports both.
const forwardedPage = `<!doctype html><title>forwarded</title><script>
${readProbe}
${writeProbe}
(async () => {
  const body = { host: location.host };
  try {
    const plan = await (await fetch("/__plan")).json();
    body.before = await readProbe();
    if (plan.tag) await writeProbe(plan.tag);
    body.after = await readProbe();
  } catch (err) {
    body.error = String(err);
  }
  await fetch("/__report", { method: "POST", body: JSON.stringify(body) });
})();
</script>`;

const server = createServer((req, res) => {
  const host = (req.headers.host ?? "").split(":")[0];
  if (req.url === "/__plan") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ tag: tagFor(host) }));
  } else if (req.url === "/__report") {
    let data = "";
    req.on("data", (chunk) => (data += chunk));
    req.on("end", () => {
      const report = JSON.parse(data);
      reports.set(report.host.split(":")[0], report);
      res.writeHead(204).end();
    });
  } else {
    res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
    res.end(/^\d+\./.test(host) ? forwardedPage : workspacePage);
  }
});
await new Promise((resolve, reject) => {
  server.once("error", reject);
  server.listen(0, "127.0.0.1", resolve);
});
const sitePort = server.address().port;

const scratch = mkdtempSync(join(tmpdir(), "puddle-origins-webkit-"));
const dataDir = join(scratch, "data");

// An id as the host draws them: a letter, then seven Crockford base32 characters.
const newId = () => `k${randomBytes(5).toString("hex").slice(0, 7)}`;

/** The pids of this run's shells: processes running `exe` with this run's data folder in their environment. */
function shellPids() {
  const pids = [];
  for (const name of readdirSync("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      if (readlinkSync(`/proc/${name}/exe`) === exe && readFileSync(`/proc/${name}/environ`, "latin1").includes(`PUDDLE_APP_DATA_DIR=${dataDir}\0`)) pids.push(Number(name));
    } catch {
      // the process is gone or is not ours
    }
  }
  return pids;
}

/** One run of the shell on workspace `id` over the shared data folder, driven through WebKitWebDriver. */
async function session(n, id, forget, body) {
  const port = driverPort + n;
  const env = {
    ...process.env,
    TAURI_WEBVIEW_AUTOMATION: "true",
    PUDDLE_APP_FIXTURE: "1",
    PUDDLE_APP_DATA_DIR: dataDir,
    PUDDLE_APP_SANDBOX_PROBE: `http://${id}.localhost:${sitePort}/`,
    PUDDLE_APP_SANDBOX_DEBUG_PORT: "0",
    PUDDLE_APP_SANDBOX_PROBE_WORKSPACE: id,
    ...(forget ? { PUDDLE_APP_SANDBOX_PROBE_FORGET: "1" } : {}),
  };
  // The shell is the driver's child, so its log (stderr) arrives on the driver's.
  const driver = spawn(driverBin, [`--port=${port}`], { stdio: ["ignore", "inherit", "pipe"], env });
  let appLog = "";
  driver.stderr.on("data", (chunk) => {
    appLog += chunk;
    process.stderr.write(chunk);
  });
  let driverExited = false;
  onChildEnd(driver, () => {
    driverExited = true;
  });
  const call = async (method, path, payload) => {
    const res = await fetch(`http://127.0.0.1:${port}${path}`, {
      method,
      headers: { "Content-Type": "application/json" },
      body: payload === undefined ? undefined : JSON.stringify(payload),
      signal: AbortSignal.timeout(120_000),
    }).catch((err) => {
      throw new Error(`${method} ${path}: ${err.message}`);
    });
    const json = await res.json();
    if (!res.ok) throw new Error(`${method} ${path}: ${JSON.stringify(json.value)}`);
    return json.value;
  };
  let session;
  const inPage = (source) =>
    call("POST", `/session/${session}/execute/async`, {
      script: `const done = arguments[arguments.length - 1]; (${source}).then(done, (err) => done({ error: String(err) }));`,
      args: [],
    }).then((value) => {
      if (value?.error) throw new Error(value.error);
      return value;
    });
  try {
    await until("the driver", async () => {
      if (driverExited) throw new GiveUp("WebKitWebDriver exited early");
      return (await fetch(`http://127.0.0.1:${port}/status`)).ok;
    });
    session = (
      await call("POST", "/session", {
        capabilities: { alwaysMatch: { browserName: "wry", "webkitgtk:browserOptions": { binary: exe, args: [] } } },
      })
    ).sessionId;
    // The window is listed from its first navigation: wait for the workspace's page itself.
    await until(`the window on ${hostsOf(id).workspace}`, async () => {
      if (driverExited) throw new GiveUp("WebKitWebDriver exited early");
      return (await call("GET", `/session/${session}/url`)).startsWith(`http://${hostsOf(id).workspace}:${sitePort}/`);
    }, 120_000);
    reports.clear();
    for (const p of FORWARDED) {
      const link = await call("POST", `/session/${session}/element`, { using: "css selector", value: `#p${p}` });
      await call("POST", `/session/${session}/element/${Object.values(link)[0]}/click`, {});
      await until(`the report of ${hostsOf(id)[p]}`, async () => reports.get(hostsOf(id)[p]), 120_000);
    }
    await body({ log: () => appLog, inPage, reports });
    await until("the session's local storage writes to reach the disk", async () => [...unsaved].every(savedLocally), 30_000);
    unsaved.clear();
  } finally {
    // Stopping the driver closes the shell's window and ends the shell; wait for it, so the web
    // process has written its storage before the next session reads.
    if (session) await call("DELETE", `/session/${session}`).catch(() => undefined);
    driver.kill();
    await until(`session ${n}'s shell to exit`, async () => shellPids().length === 0, 60_000).catch((err) => {
      check(`session ${n}: the shell exited`, false, String(err));
      for (const pid of shellPids()) process.kill(pid, "SIGKILL");
    });
  }
}

/** What each window holds: its origin and the three kinds of storage. */
async function snapshot(id, { inPage, reports }) {
  const all = { workspace: await inPage(`(${readProbe})()`) };
  for (const p of FORWARDED) {
    const report = reports.get(hostsOf(id)[p]);
    if (report.error) throw new Error(`the ${p} window failed: ${report.error}`);
    all[p] = report.before;
  }
  return all;
}

function expectEmpty(label, id, all) {
  for (const [which, host] of Object.entries(hostsOf(id))) {
    const s = all[which];
    check(`${label}: ${host} is its own origin`, s.origin === `http://${host}:${sitePort}`, s.origin);
    check(`${label}: ${host} starts with no local storage, IndexedDB or cookie`, s.local === null && s.idb === null && s.cookie === null, JSON.stringify(s));
  }
}

function expectKept(label, id, all) {
  for (const which of Object.keys(hostsOf(id))) {
    const s = all[which];
    const tag = tagOf(id, which);
    check(`${label}: ${hostsOf(id)[which]} still has its local storage, IndexedDB and cookie`, s.local === tag && s.idb === tag && s.cookie === tag, JSON.stringify(s));
  }
}

/** Writes on the workspace's window now; the forwarded pages write on load, for the tags `tagFor` hands out. */
async function writeWorkspace(id, { inPage }) {
  const origin = await inPage(`(${writeProbe})(${JSON.stringify(tagOf(id, "workspace"))})`);
  check(`${id} workspace: wrote on ${hostsOf(id).workspace}`, origin === `http://${hostsOf(id).workspace}:${sitePort}`, origin);
  unsaved.add(tagOf(id, "workspace"));
}

function expectForwardedWrites(id, reports) {
  for (const p of FORWARDED) {
    const after = reports.get(hostsOf(id)[p])?.after;
    check(`${id} ${p}: wrote on ${hostsOf(id)[p]}`, after?.origin === `http://${hostsOf(id)[p]}:${sitePort}` && after.local === tagOf(id, p), JSON.stringify(after));
    unsaved.add(tagOf(id, p));
  }
}

/** The local storage values this session wrote and WebKit has not yet been seen to keep on disk. */
const unsaved = new Set();

/** Whether the local storage files under the data folder hold `value`. */
function savedLocally(value) {
  const files = [];
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) walk(path);
      else if (/\.localstorage(-wal)?$/.test(entry.name)) files.push(path);
    }
  };
  walk(join(dataDir, "webview"));
  const utf16 = Buffer.from(value, "utf16le");
  return files.some((file) => {
    const bytes = readFileSync(file);
    return bytes.includes(value) || bytes.includes(utf16);
  });
}

const first = newId();
const second = newId();
let run = 0;
// Sessions that write: the forwarded pages are told the tag of their own host.
const writingFor = (id) => (host) => {
  const which = Object.entries(hostsOf(id)).find(([, h]) => h === host)?.[0];
  return which ? tagOf(id, which) : null;
};
const existsData = (id) => existsSync(join(dataDir, "webview", id)) && existsSync(join(dataDir, "webview", `${id}.forward`));
try {
  tagFor = writingFor(first);
  await session(run++, first, false, async (ctx) => {
    expectEmpty("1 create", first, await snapshot(first, ctx));
    await writeWorkspace(first, ctx);
    expectForwardedWrites(first, ctx.reports);
    const now = await snapshot(first, { ...ctx, reports: new Map([...ctx.reports].map(([host, r]) => [host, { ...r, before: r.after }])) });
    expectKept("1 create", first, now);
    check("1 create: the workspace and its forwarded ports have data folders", existsData(first));
  });
  tagFor = () => null;
  await session(run++, first, false, async (ctx) => {
    expectKept("2 rename (same id, new session)", first, await snapshot(first, ctx));
  });
  tagFor = writingFor(second);
  await session(run++, second, false, async (ctx) => {
    check("3 recreate: the new workspace has another origin", hostsOf(second).workspace !== hostsOf(first).workspace);
    expectEmpty("3 recreate (new id, old data still on disk)", second, await snapshot(second, ctx));
    await writeWorkspace(second, ctx);
    expectForwardedWrites(second, ctx.reports);
  });
  tagFor = () => null;
  await session(run++, first, true, async (ctx) => {
    check("4 delete: the shell removed the workspace's data", /sandbox window: data removed/.test(ctx.log()), ctx.log().split("\n").filter((l) => /data not removed/.test(l))[0] ?? "");
    expectEmpty("4 delete (same id again)", first, await snapshot(first, ctx));
  });
  await session(run++, second, false, async (ctx) => {
    expectKept("5 delete of another workspace", second, await snapshot(second, ctx));
  });
} catch (err) {
  check("the origins test ran to the end", false, String(err?.stack ?? err));
} finally {
  server.close();
  for (const pid of shellPids()) process.kill(pid, "SIGKILL");
  rmSync(scratch, { recursive: true, force: true });
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\norigins test (WebKitGTK) passed");
process.exit(0);
