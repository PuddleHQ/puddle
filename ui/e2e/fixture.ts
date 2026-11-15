// SPDX-License-Identifier: AGPL-3.0-or-later
// Driving the UI fixture backend from a test: emit events, move the clock, run the scenario's
// scripts, restart the API (open event streams end) or reset the data.
//
// `test` here gives every Playwright worker a backend of its own, so a test may change anything
// without disturbing the others; each test starts from a reset. Tests that only read use the
// shared servers from playwright.config.ts instead (see support.ts).
import { test as base, type Page } from "@playwright/test";
import { type ChildProcess, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { availableParallelism, loadavg, tmpdir } from "node:os";
import { join } from "node:path";
import {
  OutputTail,
  START_TIMEOUT_MS,
  waitForBackend,
} from "../scripts/backend-start.ts";
import { throttleCpu } from "./cpu-throttle.ts";
import { FIXTURE_BIN_ENV } from "./fixture-build.ts";
import { loadFactor } from "../scripts/load-budget.ts";
import { WARM_UP_TIMEOUT_MS, within } from "./warm-up.ts";

export interface Control {
  state(): Promise<{
    scenario: string;
    now_ms: number;
    pending: number;
    scripts: string[];
    /** Ids of the pasted tokens the fake credential store keeps. */
    kept_tokens: string[];
  }>;
  /** An `Event` in its wire form, e.g. `{ type: "oom_kill", workspace, pid, process }`. */
  emit(event: Record<string, unknown>): Promise<void>;
  advance(ms: number): Promise<void>;
  /** One step: `{ do: "request", workspace, host }`, `{ do: "rule", ... }`, ... */
  step(step: Record<string, unknown>): Promise<void>;
  script(name: string): Promise<void>;
  /** Ends open event streams and serves again with the same data. */
  restart(): Promise<void>;
  /** Starts over from the running scenario, or from a built-in one. */
  reset(scenario?: string): Promise<void>;
}

export interface Backend {
  url: string;
  token: string;
  control: Control;
  /** Hands the page the token, as the desktop shell's init script does. */
  signIn(page: Page): Promise<void>;
  /**
   * Installs Playwright's fake clock at the fixture's time, so relative times agree. The clock
   * then runs on, so a page that loads slowly reads "in 2 hours" for an expiry set 3 hours
   * ahead; `behindMs` starts it that much earlier, so such a label holds for that long.
   */
  installClock(page: Page, behindMs?: number): Promise<void>;
}

/** A backend from the files `puddle-ui-fixture --connection-file <file>` wrote. */
export function backendFromFile(connectionFile: string): Backend {
  const { url, token } = JSON.parse(readFileSync(connectionFile, "utf8")) as {
    url: string;
    token: string;
  };
  const controlUrl = (
    JSON.parse(readFileSync(`${connectionFile}.control`, "utf8")) as {
      url: string;
    }
  ).url;
  async function call(
    method: "GET" | "POST",
    path: string,
    body?: unknown,
  ): Promise<Response> {
    const response = await fetch(`${controlUrl}${path}`, {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    if (!response.ok) {
      throw new Error(
        `fixture control ${method} ${path}: ${response.status} ${await response.text()}`,
      );
    }
    return response;
  }
  const control: Control = {
    state: async () =>
      (await (await call("GET", "/control/state")).json()) as Awaited<
        ReturnType<Control["state"]>
      >,
    emit: async (event) => void (await call("POST", "/control/emit", event)),
    advance: async (ms) =>
      void (await call("POST", "/control/advance", { ms })),
    step: async (step) => void (await call("POST", "/control/step", step)),
    script: async (name) =>
      void (await call("POST", `/control/script/${encodeURIComponent(name)}`)),
    restart: async () => void (await call("POST", "/control/restart")),
    reset: async (scenario) =>
      void (await call(
        "POST",
        "/control/reset",
        scenario === undefined ? undefined : { scenario },
      )),
  };
  return {
    url,
    token,
    control,
    async signIn(page) {
      await page.addInitScript((t) => {
        (window as unknown as { __PUDDLE__: { token: string } }).__PUDDLE__ = {
          token: t,
        };
      }, token);
    },
    async installClock(page, behindMs = 0) {
      await page.clock.install({
        time: (await control.state()).now_ms - behindMs,
      });
    },
  };
}

interface Started {
  backend: Backend;
  stop(): void;
}

async function startBackend(scenario: string): Promise<Started> {
  const dir = mkdtempSync(join(tmpdir(), "puddle-fixture-"));
  const file = join(dir, "connection.json");
  const executable = process.env[FIXTURE_BIN_ENV];
  if (executable === undefined) {
    throw new Error(
      `${FIXTURE_BIN_ENV} is not set: e2e/fixture-build.ts did not run`,
    );
  }
  const child: ChildProcess = spawn(
    executable,
    ["--scenario", scenario, "--connection-file", file],
    { stdio: ["ignore", "pipe", "pipe"] },
  );
  // The tail names what the backend said when it fails to start; the log still gets all of it.
  const output = new OutputTail();
  for (const stream of [child.stdout, child.stderr]) {
    stream?.on("data", (chunk: Buffer) => output.push(chunk));
  }
  child.stderr?.pipe(process.stderr);
  try {
    await waitForBackend(child, file, output, { executable });
  } catch (error) {
    child.kill();
    rmSync(dir, { recursive: true, force: true });
    throw error;
  }
  return {
    backend: backendFromFile(file),
    stop() {
      child.kill();
      rmSync(dir, { recursive: true, force: true });
    },
  };
}

/** A page open that takes longer than this is reported while it is still going. */
const SLOW_PAGE_MS = 8_000;

/** When this worker started, and how long its backend took to start. */
const workerBorn = Date.now();
let backendStartMs = -1;

/** Where a slow page open was in the worker's life, so one that never returns still leaves a line. */
function pageOpenState(browserName: string): string {
  const load = loadavg()
    .map((n) => n.toFixed(1))
    .join(" ");
  return `${browserName} worker up ${Date.now() - workerBorn} ms, backend started in ${backendStartMs} ms, load ${load}`;
}

/** How long the backend may take to start over for a test, before the test's own limit begins. */
const RESET_TIMEOUT_MS = 60_000;

export const test = base.extend<
  { backend: Backend; loadBudget: void },
  { workerBackend: Backend }
>({
  workerBackend: [
    // eslint-disable-next-line no-empty-pattern -- Playwright reads the fixtures from the destructuring
    async ({}, use) => {
      const began = Date.now();
      const started = await startBackend("default");
      backendStartMs = Date.now() - began;
      await use(started.backend);
      started.stop();
    },
    // The start has its own limit (START_TIMEOUT_MS) and says why when it is hit; without this
    // the test's 30 s ended the setup first, with a message that names nothing.
    { scope: "worker", timeout: START_TIMEOUT_MS + 15_000 },
  ],
  // The reset restarts the API on its port, which a busy host stretched from 0.1 s to 4 s, and a
  // stalled one to the whole 30 s of a test that had not begun. It has its own limit, as the page
  // open does, and a stall names itself and the load.
  backend: [
    async ({ workerBackend }, use) => {
      try {
        await within(
          RESET_TIMEOUT_MS,
          workerBackend.control.reset("default"),
          "the fixture backend's reset",
        );
      } catch (error) {
        throw new Error(`${String(error)} (${pageOpenState("this worker")})`, {
          cause: error,
        });
      }
      await use(workerBackend);
    },
    { timeout: RESET_TIMEOUT_MS + 15_000 },
  ],
  // Every step of a test costs what the machine's load makes it: the settings tests, 10 to 20
  // steps and a reload or two each, took 15 to 30 s on a starved host where an idle one needs 1 to
  // 2 s. The limit grows with the load read as the test starts (scripts/load-budget.ts).
  loadBudget: [
    // eslint-disable-next-line no-empty-pattern -- Playwright reads the fixtures from the destructuring
    async ({}, use, testInfo) => {
      testInfo.setTimeout(
        testInfo.timeout *
          loadFactor(loadavg()[0] ?? 0, availableParallelism()),
      );
      await use();
    },
    { auto: true },
  ],
  // As Playwright's own `page`, plus a line when opening it is slow: `newPage` has no time limit
  // of its own, so a stalled open showed only as "Test timeout exceeded while setting up page".
  // The open has its own limit, the one the warm-up gives the same call, so the test's 30 s are
  // spent on what it checks: a WebKit page took 8 to 14 s to open on a host at load 335 on 40
  // cores, and once over 30 s, which failed a one-second test on its setup. A page that is
  // stalled still fails, by name, after that limit.
  page: [
    async ({ context, browserName }, use) => {
      const began = Date.now();
      const slow = setTimeout(() => {
        console.warn(
          `page still not open after ${SLOW_PAGE_MS} ms: ${pageOpenState(browserName)}`,
        );
      }, SLOW_PAGE_MS);
      let page: Page;
      try {
        page = await context.newPage();
      } finally {
        clearTimeout(slow);
      }
      if (Date.now() - began >= SLOW_PAGE_MS) {
        console.warn(
          `page opened after ${Date.now() - began} ms: ${pageOpenState(browserName)}`,
        );
      }
      await throttleCpu(page, browserName);
      await use(page);
    },
    { scope: "test", timeout: WARM_UP_TIMEOUT_MS },
  ],
  baseURL: async ({ workerBackend }, use) => {
    await use(workerBackend.url);
  },
});
export { expect } from "@playwright/test";
