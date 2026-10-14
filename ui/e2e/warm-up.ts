// SPDX-License-Identifier: GPL-3.0-or-later
// Playwright's global setup: starts each project's browser once and opens the app in it, before
// any test runs.
//
// The first page a freshly started machine's browser opens is slow: on a Windows runner the first
// two tests of a run took 5 to 8.5 s in 8 of 8 runs, against about 1 s for the rest, and once the
// first page took over 30 s. Without this, the first test of each worker pays for it and fails with
// "Test timeout exceeded while setting up page" though nothing it checks is wrong. Here the start
// has limits of its own (a stalled start is tried again, a start that stalls every time fails by
// naming itself), and the tests keep their 30 s for what they check.
//
// The line this logs splits the time by phase. On a cold Windows runner the close phase is often
// 15 to 18 s while the first page takes 2 to 4 s: Edge's `identity_helper.exe` starts as the
// browser exits and runs about 15 s, and the crash handler (and Playwright's wait for the
// browser's output pipes) wait for it. That is Edge's own first-run cost and not a stall of the
// first page; do not count it as one.
//
// The retry is a stopgap for one 30 s first-page stall that has not been reproduced since (in 99
// cold starts on fresh runners the first page took 1.2 to 11.8 s, 20 of them with every core
// busy). Remove the retry, and keep the limits, once the logged first-page phase has stayed under
// 15 s in 100 consecutive Windows UI runs with no "try 2" line.
import {
  chromium,
  firefox,
  webkit,
  type FullConfig,
  type FullProject,
} from "@playwright/test";
import { firstStart, PhaseTimer } from "../tools/first-start.ts";

/** How long one try at the first page may take, and how many tries there are; the tests' own limit stays what it is. */
export const WARM_UP_TIMEOUT_MS = 45_000;
export const WARM_UP_TRIES = 3;

const engines = { chromium, firefox, webkit } as const;

/** `newPage` takes no timeout of its own: without this a stalled browser would hold the run forever. */
async function within<T>(
  ms: number,
  work: Promise<T>,
  what: string,
): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  const expired = new Promise<never>((_, fail) => {
    timer = setTimeout(() => fail(new Error(`${what} took over ${ms} ms`)), ms);
  });
  try {
    return await Promise.race([work, expired]);
  } finally {
    clearTimeout(timer);
  }
}

/** Starts the project's browser as its tests do, opens `baseURL` in a page and closes both. */
async function openOnce(project: FullProject): Promise<string> {
  const { defaultBrowserType, channel, launchOptions, baseURL } = project.use;
  const engine = engines[defaultBrowserType ?? "chromium"];
  const phases = new PhaseTimer();
  try {
    const browser = await phases.run("launch", () =>
      engine.launch({
        ...launchOptions,
        ...(channel === undefined ? {} : { channel }),
        timeout: WARM_UP_TIMEOUT_MS,
      }),
    );
    try {
      const page = await phases.run("first page", () =>
        within(
          WARM_UP_TIMEOUT_MS,
          browser.newPage({ ...(baseURL === undefined ? {} : { baseURL }) }),
          "opening the first page",
        ),
      );
      await phases.run("goto", () =>
        page.goto("/", { timeout: WARM_UP_TIMEOUT_MS }),
      );
    } finally {
      // A stalled browser may not close either; the next try must still come.
      await phases
        .run("close", () =>
          within(WARM_UP_TIMEOUT_MS, browser.close(), "closing it"),
        )
        .catch((error: unknown) => {
          console.warn(`warm-up: ${String(error)}`);
        });
    }
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    throw new Error(`${reason} (${phases.describe()})`, { cause: error });
  }
  return phases.describe();
}

export default async function warmUp(config: FullConfig): Promise<void> {
  for (const project of config.projects) {
    const started = Date.now();
    const phases = await firstStart({
      what: `the ${project.name} browser`,
      tries: WARM_UP_TRIES,
      attempt: () => openOnce(project),
      log: (line) => {
        console.warn(`warm-up: ${line}`);
      },
    });
    console.warn(
      `warm-up: ${project.name} opened the app in ${Date.now() - started} ms (${phases})`,
    );
  }
}
