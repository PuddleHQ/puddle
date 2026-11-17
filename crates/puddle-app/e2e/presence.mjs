// SPDX-License-Identifier: AGPL-3.0-or-later
// Whether a minimised or hidden desktop window keeps reporting its presence. The page reports to
// `POST /api/presence` every 30 s from a timer (`ui/src/lib/presence/reporter.ts`), and the service
// counts a client silent for 75 s as gone, so a webview that throttles a hidden page's timers would
// make the server stop picking the app while a browser tab elsewhere is live.
//
// The main window runs on the fixture backend, driven over WebView2's DevTools Protocol like
// smoke.mjs; each report is seen as a Network event, timed on arrival. After a visible baseline the
// window is minimised (ShowWindow, the taskbar button's effect) for 90 s, then closed with the
// title bar's close message, which hides it in the tray (the default close behaviour) for another
// 90 s. Over the whole 3 minutes no two reports may be more than 75 s apart, and the last one may be
// no older than that when the test ends.
//
// Environment: PUDDLE_APP_EXE (a debug build with all features), CDP_PORT (default 9350),
// PRESENCE_PHASE_S (seconds per phase, default 90). Plain Node 24, no packages.
import { spawn, spawnSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import { fileURLToPath } from "node:url";
import { connect } from "./cdp.mjs";
import { onChildEnd } from "./child-end.mjs";

const exe = process.env.PUDDLE_APP_EXE;
if (!exe) throw new Error("set PUDDLE_APP_EXE to the built puddle-app");
const port = Number(process.env.CDP_PORT ?? 9350);
const phaseMs = Number(process.env.PRESENCE_PHASE_S ?? 90) * 1000;
const STALE_MS = 75_000;
const helper = fileURLToPath(new URL("./presence-window.ps1", import.meta.url));

const failures = [];
function check(name, ok, detail = "") {
  console.log(`${ok ? "ok  " : "FAIL"} ${name}${detail ? `: ${detail}` : ""}`);
  if (!ok) failures.push(name);
}

/** Polls `probe` until it returns something truthy, for `within` ms. */
async function until(what, probe, within = 60_000) {
  const deadline = Date.now() + within;
  for (;;) {
    const value = await probe().catch(() => undefined);
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(250);
  }
}

function windowAction(action, pid) {
  const run = spawnSync("pwsh", ["-NoProfile", "-File", helper, action, String(pid)], { encoding: "utf8", timeout: 60_000 });
  if (run.status !== 0) throw new Error(`${action} failed (${run.status}): ${run.stderr || run.error}`);
  return run.stdout.trim();
}

const app = spawn(exe, [], { stdio: ["ignore", "inherit", "pipe"], env: { ...process.env, PUDDLE_APP_FIXTURE: "1", PUDDLE_APP_DEBUG_PORT: String(port) } });
let exited = false;
app.stderr.on("data", (chunk) => process.stderr.write(chunk));
const done = new Promise((resolve) =>
  onChildEnd(app, () => {
    exited = true;
    resolve();
  }),
);

/** One entry per report the page sent: when it arrived here, and what it said. */
const reports = [];
let page;
try {
  const target = await until("the main window", async () => {
    if (exited) throw new Error("puddle-app exited early");
    const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    return list.find((t) => t.type === "page" && /^http:\/\/127\.0\.0\.1:\d+\//.test(t.url));
  });
  page = await connect(target.webSocketDebuggerUrl);
  page.on("Network.requestWillBeSent", ({ request }) => {
    if (request.method !== "POST" || !new URL(request.url).pathname.endsWith("/api/presence")) return;
    let body = {};
    try {
      body = JSON.parse(request.postData ?? "{}");
    } catch {
      // the timing is what counts
    }
    reports.push({ at: Date.now(), visible: body.visible, kind: body.kind });
  });
  await page.send("Network.enable");
  // The reporter starts when the layout mounts, which may be before Network was enabled; a reload
  // makes the first report certain to be seen, and the next one is the timer's.
  await page.send("Page.reload");

  await until("the first report", async () => reports.length >= 1, 60_000);
  check("the desktop window reports as kind desktop", reports[0].kind === "desktop", JSON.stringify(reports[0]));
  const timerSeen = await until("a second report from the timer", async () => reports.length >= 2, 45_000).catch(() => false);
  check("a visible window reports on its 30 s timer", timerSeen && reports[1].at - reports[0].at <= STALE_MS, JSON.stringify(reports));
  const start = reports.at(-1).at;

  windowAction("minimize", app.pid);
  const minimised = await until("the window to be minimised or hidden", async () => /visible=False|minimized=True/.test(windowAction("state", app.pid)), 15_000).catch(() => false);
  check("the window is minimised", minimised, windowAction("state", app.pid));
  await sleep(phaseMs);

  windowAction("close", app.pid);
  const hidden = await until("the window to be hidden", async () => /visible=False/.test(windowAction("state", app.pid)), 15_000).catch(() => false);
  check("the close button hides the window in the tray", hidden, windowAction("state", app.pid));
  check("the program runs on", !exited);
  await sleep(phaseMs);

  const end = Date.now();
  const times = reports.map((r) => r.at).filter((t) => t >= start);
  const gaps = times.slice(1).map((t, i) => t - times[i]);
  gaps.push(end - times.at(-1));
  const worst = Math.max(...gaps);
  console.log(`     ${times.length} reports in ${Math.round((end - start) / 1000)} s, gaps (s): ${gaps.map((g) => Math.round(g / 1000)).join(" ")}`);
  console.log(`     visible flags: ${reports.map((r) => r.visible).join(" ")}`);
  check("reports kept coming while minimised and hidden", times.length >= Math.floor((2 * phaseMs) / 30_000) - 1, `${times.length} reports`);
  check(`no gap over ${STALE_MS / 1000} s in the reports`, worst <= STALE_MS, `worst gap ${Math.round(worst / 1000)} s`);
} catch (err) {
  check("the test ran to the end", false, String(err?.stack ?? err));
} finally {
  page?.close();
  if (!exited) spawnSync("taskkill", ["/PID", String(app.pid), "/T", "/F"], { stdio: "ignore" });
  await Promise.race([done, sleep(5000)]);
}

if (failures.length > 0) {
  console.error(`\n${failures.length} check(s) failed: ${failures.join("; ")}`);
  process.exit(1);
}
console.log("\npresence test passed");
process.exit(0);
