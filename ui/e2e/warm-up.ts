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
import {
  chromium,
  firefox,
  webkit,
  type FullConfig,
  type FullProject,
} from "@playwright/test";
import { firstStart } from "../tools/first-start.ts";

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
async function openOnce(project: FullProject): Promise<void> {
  const { defaultBrowserType, channel, launchOptions, baseURL } = project.use;
  const engine = engines[defaultBrowserType ?? "chromium"];
  const browser = await engine.launch({
    ...launchOptions,
    ...(channel === undefined ? {} : { channel }),
    timeout: WARM_UP_TIMEOUT_MS,
  });
  try {
    const page = await within(
      WARM_UP_TIMEOUT_MS,
      browser.newPage({ ...(baseURL === undefined ? {} : { baseURL }) }),
      "opening the first page",
    );
    await page.goto("/", { timeout: WARM_UP_TIMEOUT_MS });
  } finally {
    // A stalled browser may not close either; the next try must still come.
    await within(WARM_UP_TIMEOUT_MS, browser.close(), "closing it").catch(
      (error: unknown) => {
        console.warn(`warm-up: ${String(error)}`);
      },
    );
  }
}

export default async function warmUp(config: FullConfig): Promise<void> {
  for (const project of config.projects) {
    const started = Date.now();
    await firstStart({
      what: `the ${project.name} browser`,
      tries: WARM_UP_TRIES,
      attempt: () => openOnce(project),
      log: (line) => {
        console.warn(`warm-up: ${line}`);
      },
    });
    console.warn(
      `warm-up: ${project.name} opened the app in ${Date.now() - started} ms`,
    );
  }
}
