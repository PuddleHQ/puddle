// SPDX-License-Identifier: AGPL-3.0-or-later
// End-to-end tests against the UI fixture backend: the real puddle-api serving the built
// app on 127.0.0.1 (the same origin the desktop shell loads), on fake services with seeded data.
// This config starts one shared, read-only server (scenario `default`: one pending request).
// A test that changes state uses e2e/fixture.ts instead, which gives each worker its own.
//   Linux: Chromium and WebKit (WebKit stands in for WebKitGTK and, later, WKWebView).
//   Windows: the installed Edge (WebView2's engine), so no browser download.
import { defineConfig, devices } from "@playwright/test";
import { createServer } from "node:net";
import { resolve } from "node:path";
import { slowdown } from "./e2e/cpu-throttle.ts";

// A free port per run, so gate runs from several checkouts can overlap. Playwright loads this
// config again in every worker, so the first load (the runner's) picks the port and passes it on
// through the environment.
async function freePort(): Promise<number> {
  return new Promise((done, fail) => {
    const probe = createServer();
    probe.once("error", fail);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      probe.close(() =>
        typeof address === "object" && address !== null
          ? done(address.port)
          : fail(new Error("no port")),
      );
    });
  });
}
const PORT = Number(
  (process.env["PUDDLE_E2E_PORT"] ??= String(await freePort())),
);
const CONNECTION_FILE = resolve(
  import.meta.dirname,
  ".svelte-kit",
  "e2e",
  "connection.json",
);
process.env["PUDDLE_E2E_CONNECTION"] = CONNECTION_FILE;

// `check.sh` passes its cargo wrapper (mbx) here; plain cargo otherwise.
const cargo = process.env["PUDDLE_CARGO"] ?? "cargo";
const windows = process.platform === "win32";
// Narrow the browsers, e.g. PUDDLE_E2E_PROJECTS=chromium (check.sh does this where WebKit can't start).
const only = (process.env["PUDDLE_E2E_PROJECTS"] ?? "")
  .split(",")
  .filter(Boolean);

// Playwright starts half the logical CPUs as workers. The Linux host shares its cores between
// several runs at once, so it passes the share one run may use here (remote-check-host.sh).
const workers = Number(process.env["PUDDLE_E2E_WORKERS"]);

export default defineConfig({
  testDir: "e2e",
  fullyParallel: true,
  ...(Number.isInteger(workers) && workers > 0 ? { workers } : {}),
  // Builds the fixture backend once (e2e/fixture-build.ts), then starts each browser once before
  // the tests (e2e/warm-up.ts).
  globalSetup: ["./e2e/fixture-build.ts", "./e2e/warm-up.ts"],
  forbidOnly: !!process.env["CI"],
  retries: 0,
  reporter: process.env["CI"]
    ? [["list"], ["html", { open: "never" }]]
    : "list",
  // The nightly stress job (ci/stress.sh) declares a slowdown (`PUDDLE_E2E_CPU_THROTTLE`, 1 everywhere
  // else) and starves the browser's CPU. The page's own start, which runs before its first request,
  // took 7 s at the median and 13 s at the worst of 40 loads there against 2 s unstarved, and every
  // later step is dearer by about the same factor, so a test's and a read's limit fail a page that
  // is only slow. Both limits grow by the slowdown the run declares, as the bars of `inbox-bulk` and
  // `inbox-full-list` do; a run that declares none keeps 90 s and 30 s.
  //
  // The 30 s these limits were is a wall clock on processes the host may not schedule. Seven host logs
  // fail the same way (a test or its `beforeEach` timed out at 30 s, four workers at once, tests that
  // take 1 to 3 s idle) with the host's load average at 30 to 314 on 40 cores, and the `ui` gate's
  // vitest probe in the same run reported a worker stalled for 7.6 s. A passing test returns at once, so
  // a long limit costs nothing; a hung one still fails, after 90 s. Raising the limit of one step at a
  // time (the page open, the backend start) had moved the failure to the next step each time.
  timeout: 90_000 * slowdown(),
  // `expect.poll` and the web-first assertions give a read this long. The 5 s default is shorter than an
  // axe scan on a loaded machine (a scan that has not returned in 5 s ends a poll with "waiting on the
  // predicate"), so a loaded run failed on time, not on the page.
  expect: { timeout: 30_000 * slowdown() },
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    trace: "retain-on-failure",
  },
  projects: (windows
    ? [
        {
          name: "msedge",
          use: {
            ...devices["Desktop Edge"],
            channel: "msedge",
          },
        },
      ]
    : [
        { name: "chromium", use: { ...devices["Desktop Chrome"] } },
        { name: "webkit", use: { ...devices["Desktop Safari"] } },
      ]
  ).filter((project) => only.length === 0 || only.includes(project.name)),
  webServer: {
    command: `${cargo} run --quiet --locked -p puddle-e2e --features embedded-ui --bin puddle-ui-fixture -- ${PORT} "${CONNECTION_FILE}"`,
    cwd: resolve(import.meta.dirname, ".."),
    url: `http://127.0.0.1:${PORT}/`,
    reuseExistingServer: false,
    timeout: 600_000,
    stdout: "pipe",
  },
});
