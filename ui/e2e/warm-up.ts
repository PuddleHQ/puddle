// SPDX-License-Identifier: GPL-3.0-or-later
// Playwright's global setup: starts each project's browser once and opens the app in it, before
// any test runs.
//
// The first page a freshly started machine's browser opens is slow: on a Windows runner the first
// two tests of a run took 5 to 8.5 s in 8 of 8 runs, against about 1 s for the rest, and once the
// first page took over 30 s. Without this, the first test of each worker pays for it and fails with
// "Test timeout exceeded while setting up page" though nothing it checks is wrong. Here the start
// has limits of its own (a start that stalls fails by naming itself and the phase that stalled),
// and the tests keep their 30 s for what they check.
//
// The line this logs splits the time by phase. On a cold Windows runner the close phase is often
// 15 to 18 s while the first page takes 2 to 4 s: Edge's `identity_helper.exe` starts as the
// browser exits and runs about 15 s, and the crash handler (and Playwright's wait for the
// browser's output pipes) wait for it. That is Edge's own first-run cost and not a stall of the
// first page; do not count it as one.
//
// There is no retry: in 124 consecutive Windows UI runs (2026-10-24 to 10-27, temp folder on D:)
// the first page took under 15 s every time. Before that, 6 of 257 runs stalled 23 s or more.
// A stall now fails the run with the phase that took the time; add a retry back only if it recurs.
import {
  chromium,
  firefox,
  webkit,
  type FullConfig,
  type FullProject,
} from "@playwright/test";
import { PhaseTimer } from "../tools/first-start.ts";

/** How long each phase of the start may take; the tests' own limit stays what it is. */
export const WARM_UP_TIMEOUT_MS = 45_000;

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
      // A stalled browser may not close either; the run must still end with the reason.
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
    let phases: string;
    try {
      phases = await openOnce(project);
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error);
      throw new Error(
        `the ${project.name} browser did not open the app: ${reason}`,
        { cause: error },
      );
    }
    console.warn(
      `warm-up: ${project.name} opened the app in ${Date.now() - started} ms (${phases})`,
    );
  }
}
