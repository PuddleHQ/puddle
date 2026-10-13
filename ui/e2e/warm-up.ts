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
export async function warmUpProject(project: FullProject): Promise<number> {
  const { defaultBrowserType, channel, launchOptions, baseURL } = project.use;
  const engine = engines[defaultBrowserType ?? "chromium"];
  const started = Date.now();
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
    await browser.close();
  }
  return Date.now() - started;
}

export default async function warmUp(config: FullConfig): Promise<void> {
  for (const project of config.projects) {
    const ms = await warmUpProject(project).catch((error: unknown) => {
      throw new Error(
        `warm-up of the ${project.name} browser failed: ${String(error)}`,
      );
    });
    console.warn(`warm-up: ${project.name} opened the app in ${ms} ms`);
  }
}
