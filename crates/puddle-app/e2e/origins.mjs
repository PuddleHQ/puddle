// SPDX-License-Identifier: AGPL-3.0-or-later
// What a real webview (WebView2, so Windows only) keeps for a workspace's origins, which the host's
// fake-launcher tests can only assume: local storage, IndexedDB and a cookie on `<id>.localhost`
// and on a forwarded port's `<port>.<id>.localhost`, kept under the workspace's own data folder.
// Driven over WebView2's DevTools Protocol like sandbox-window.mjs, plain Node 24, no packages.
//
// The shell runs on the fixture backend with a probe window standing in for the workspace's editor
// (PUDDLE_APP_SANDBOX_PROBE_WORKSPACE names the id, PUDDLE_APP_SANDBOX_PROBE_FORGET=1 removes the
// workspace's webview data first, as a delete does), over one data folder, in sessions:
//   1. create: `<id>.localhost` and two forwarded ports start empty, take a write each, and read
//      back only their own (the forwarded ports share one data folder but not an origin).
//   2. rename: a rename changes the display name and never the id, the origin or the data folder,
//      so a new session on the same id and folder (as after a rename and a restart) finds it all.
//   3. recreate: a workspace made later has another id, so another `<id>.localhost` origin and
//      empty storage on all three origins, whatever the first one left on disk.
//   4. delete: the first id's data is removed (the shell logs it), and the same id then shows
//      empty storage on all three origins, while the second workspace's data stays.
// A session ends by closing its windows and waiting for the webview's browser process to exit, which
// is what makes it write its cookies and local storage to disk; a kill would lose the last seconds.
// What it does not show: the host choosing the ids and deleting the folders (the host tests do that
// with real files), and WebKitGTK (no DevTools Protocol).
//
// Environment: PUDDLE_APP_EXE (a debug build with all features: the in-memory fixture backend),
// CDP_PORT (default 9336; each session uses the next two ports).
import { spawn, spawnSync } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, mkdtempSync, readdirSync, rmSync } from "node:fs";
import { DatabaseSync } from "node:sqlite";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { randomBytes } from "node:crypto";
import { setTimeout as sleep } from "node:timers/promises";
import { connect } from "./cdp.mjs";
import { onChildEnd } from "./child-end.mjs";

const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
const basePort = Number(process.env.CDP_PORT ?? 9336);
const FORWARDED = [5173, 5174];

// Cookie hosts this session wrote, with the data folder that must come to hold them on disk.
let written = [];

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

/**
 * Polls `probe` until it returns something truthy, for `within` ms. DevTools has no event for "the
 * window is listed", "the page has loaded" or "the process has exited", so these are bounded polls
 * with the reason at each call, never a fixed sleep.
 */
async function until(what, probe, within = 60_000) {
  const deadline = Date.now() + within;
  for (;;) {
    const value = await probe().catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(250);
  }
}

// Every path of every host answers with a blank page: the origin is the point, not the content.
const server = createServer((_req, res) => {
  res.writeHead(200, { "Content-Type": "text/html; charset=utf-8" });
  res.end("<!doctype html><title>origin probe</title>");
});
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const sitePort = server.address().port;

const scratch = mkdtempSync(join(tmpdir(), "puddle-origins-"));
const webview = join(scratch, "data", "webview");

// An id as the host draws them: a letter, then seven Crockford base32 characters.
const newId = () => `k${randomBytes(5).toString("hex").slice(0, 7)}`;

const write = (tag) => `(async () => {
  localStorage.setItem("probe", ${JSON.stringify(tag)});
  await new Promise((resolve, reject) => {
    const open = indexedDB.open("probe", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("kv");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const tx = open.result.transaction("kv", "readwrite");
      tx.objectStore("kv").put(${JSON.stringify(tag)}, "probe");
      tx.oncomplete = () => { open.result.close(); resolve(); };
      tx.onerror = () => reject(tx.error);
    };
  });
  document.cookie = "probe=" + ${JSON.stringify(tag)} + "; path=/; max-age=3600";
  return location.origin;
})()`;

const read = `(async () => {
  const idb = await new Promise((resolve, reject) => {
    const open = indexedDB.open("probe", 1);
    open.onupgradeneeded = () => open.result.createObjectStore("kv");
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const get = open.result.transaction("kv").objectStore("kv").get("probe");
      get.onsuccess = () => { open.result.close(); resolve(get.result ?? null); };
      get.onerror = () => reject(get.error);
    };
  });
  const cookie = document.cookie.split("; ").find((c) => c.startsWith("probe="));
  return { origin: location.origin, local: localStorage.getItem("probe"), idb, cookie: cookie ? cookie.slice(6) : null };
})()`;

const describe = `({
  href: location.href,
  origin: location.origin,
  readyState: document.readyState,
  secure: isSecureContext,
  cookies: navigator.cookieEnabled,
  idb: typeof indexedDB,
})`;

const listPages = async (port) => (await (await fetch(`http://127.0.0.1:${port}/json/list`)).json()).filter((t) => t.type === "page");

/** The `Cookies` SQLite file somewhere under a webview data folder (its subpath varies by version). */
function findCookieDb(dir) {
  if (!existsSync(dir)) return null;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) {
      const found = findCookieDb(path);
      if (found) return found;
    } else if (entry.name === "Cookies") return path;
  }
  return null;
}

/** Whether the cookie store under `dir` has a row for the probe cookie of `host`. */
function cookieOnDisk(dir, host) {
  const file = findCookieDb(dir);
  if (!file) return false;
  const db = new DatabaseSync(file, { readOnly: true });
  try {
    return db.prepare("select count(*) as n from cookies where host_key = ? and name = 'probe'").get(host).n > 0;
  } finally {
    db.close();
  }
}

/** One run of the shell on workspace `id` over the shared data folder. */
async function session(n, id, forget, body) {
  const port = basePort + 2 * n;
  const env = {
    ...process.env,
    PUDDLE_APP_FIXTURE: "1",
    PUDDLE_APP_DATA_DIR: join(scratch, "data"),
    PUDDLE_APP_SANDBOX_PROBE: `http://${id}.localhost:${sitePort}/`,
    PUDDLE_APP_SANDBOX_DEBUG_PORT: String(port),
    PUDDLE_APP_SANDBOX_PROBE_WORKSPACE: id,
    ...(forget ? { PUDDLE_APP_SANDBOX_PROBE_FORGET: "1" } : {}),
  };
  const app = spawn(exe, [], { stdio: ["ignore", "inherit", "pipe"], env });
  let appLog = "";
  app.stderr.on("data", (chunk) => {
    appLog += chunk;
    process.stderr.write(chunk);
  });
  let exited = false;
  onChildEnd(app, () => {
    exited = true;
  });
  const opened = [];
  // A page on `host` (the workspace's origin or a forwarded port's) on `debug`'s browser process,
  // listed from its first navigation, so also wait for the page to have loaded.
  const pageOn = async (debug, host) => {
    const target = await until(`a window on ${host}`, async () => {
      if (exited) throw new Error("puddle-app exited early");
      return (await listPages(debug)).find((t) => t.url.startsWith(`http://${host}:${sitePort}/`));
    });
    const page = await connect(target.webSocketDebuggerUrl);
    opened.push(page);
    // A window is listed at the address it is navigating to, while its document is still the
    // initial about:blank (already "complete"), so readyState alone does not say the page loaded:
    // wait for the document to be the target's, and on a timeout say what the window shows.
    await until(`${host} to load`, async () => {
      const now = await page.evaluate(describe);
      return now.href === target.url && now.readyState === "complete";
    }).catch(async (err) => {
      throw new Error(`${err.message}; the window shows ${JSON.stringify(await page.evaluate(describe).catch(() => null))} for ${target.url}`);
    });
    // Which document each storage call will run in, printed before the first one so a storage
    // error names its window (an error page or an opaque origin would show here).
    console.log(`     ${host} window: ${JSON.stringify({ targetUrl: target.url, ...(await page.evaluate(describe)) })}`);
    return page;
  };
  try {
    const workspace = await pageOn(port, `${id}.localhost`);
    const forwarded = {};
    for (const p of FORWARDED) {
      // The guest's /proxy/<port>/ links open a window on that port's own origin.
      await workspace.evaluate(`window.open("/proxy/${p}/"); true`, true);
      forwarded[p] = await pageOn(port + 1, `${p}.${id}.localhost`);
    }
    await body({
      log: () => appLog,
      pages: { workspace, ...Object.fromEntries(FORWARDED.map((p) => [p, forwarded[p]])) },
    });
  } finally {
    // Closing the windows ends the browser processes (one per data folder), which write their
    // cookies and storage on the way out; their debug ports going quiet says they have.
    for (const page of opened) await page.send("Page.close").catch(() => undefined);
    for (const debug of [port, port + 1]) {
      await until(`the browser process on port ${debug} to exit`, async () => {
        try {
          await fetch(`http://127.0.0.1:${debug}/json/version`);
          return false;
        } catch {
          return true;
        }
      }, 30_000).catch((err) => check(`session ${n}: the webview flushed and exited`, false, String(err)));
    }
    // A debug port going quiet says the browser process is going, not that its network service has
    // finished writing the cookie store (a cookie is committed lazily and flushed on the way out),
    // so wait until each cookie this session wrote is on disk before the next session reads.
    for (const { host, dir } of written) {
      await until(`the cookie of ${host} on disk`, async () => cookieOnDisk(dir, host), 30_000).catch((err) => check(`session ${n}: the webview flushed ${host}'s cookie`, false, String(err)));
    }
    written = [];
    for (const page of opened) page.close();
    if (!exited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
    await until("puddle-app to exit", async () => exited, 30_000).catch(() => undefined);
  }
}

const hostsOf = (id) => ({ workspace: `${id}.localhost`, ...Object.fromEntries(FORWARDED.map((p) => [p, `${p}.${id}.localhost`])) });
const tagOf = (id, which) => `${which}-of-${id}`;

/** What each page holds: its origin and the three kinds of storage. */
async function snapshot(pages) {
  const all = {};
  for (const [which, page] of Object.entries(pages)) {
    all[which] = await page.evaluate(read).catch(async (err) => {
      throw new Error(`reading the ${which} window ${JSON.stringify(await page.evaluate(describe).catch(() => null))}: ${err.message}`);
    });
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

async function writeAll(id, pages) {
  for (const which of Object.keys(hostsOf(id))) {
    const origin = await pages[which].evaluate(write(tagOf(id, which)));
    written.push({ host: hostsOf(id)[which], dir: join(webview, which === "workspace" ? id : `${id}.forward`) });
    check(`${id} ${which}: wrote on ${hostsOf(id)[which]}`, origin === `http://${hostsOf(id)[which]}:${sitePort}`, origin);
  }
}

const first = newId();
const second = newId();
let run = 0;
try {
  await session(run++, first, false, async ({ pages }) => {
    expectEmpty("1 create", first, await snapshot(pages));
    await writeAll(first, pages);
    expectKept("1 create", first, await snapshot(pages));
    check("1 create: the workspace and its forwarded ports have data folders", existsSync(join(webview, first)) && existsSync(join(webview, `${first}.forward`)));
  });
  await session(run++, first, false, async ({ pages }) => {
    expectKept("2 rename (same id, new session)", first, await snapshot(pages));
  });
  await session(run++, second, false, async ({ pages }) => {
    check("3 recreate: the new workspace has another origin", hostsOf(second).workspace !== hostsOf(first).workspace);
    expectEmpty("3 recreate (new id, old data still on disk)", second, await snapshot(pages));
    await writeAll(second, pages);
  });
  await session(run++, first, true, async ({ pages, log }) => {
    check("4 delete: the shell removed the workspace's data", /sandbox window: data removed/.test(log()), log().split("\n").filter((l) => /data not removed/.test(l))[0] ?? "");
    expectEmpty("4 delete (same id again)", first, await snapshot(pages));
  });
  await session(run++, second, false, async ({ pages }) => {
    expectKept("5 delete of another workspace", second, await snapshot(pages));
  });
} catch (err) {
  check("the origins test ran to the end", false, String(err?.stack ?? err));
} finally {
  server.close();
  // WebView2's child processes can still hold the data folder for a moment after the kill; a
  // leftover temp folder is not a result of this test.
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
console.log("\norigins test passed");
process.exit(0);
