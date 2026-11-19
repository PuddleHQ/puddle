// SPDX-License-Identifier: AGPL-3.0-or-later
// A page load whose request gets no answer at all. The nightly stress run (WebKit on two starved
// cores) had `page.goto` sit on a request with no response for the whole 24 minutes of the test's
// limit (audit-retention, 2026-11-19: trace status -1, the same test and the same backend answered
// in 2 s on the other 2014 loads). A response head is the cheap part of a load, so waiting for it is
// not a speed limit like the load event: when it does not come, the request is lost, and the same
// navigation sent again is a new request on a new connection. That is tried once, loudly; a second
// silence fails by name with what a plain fetch of the same address returned meanwhile, so the next
// occurrence says whether the backend or the browser went quiet.

export interface Attempt<T> {
  /** The navigation itself, as `page.goto` returns it. */
  settled: Promise<T>;
  /** Resolves true when the navigation's response head arrived, false when `limitMs` passed without one. */
  answered: Promise<boolean>;
}

export interface WatchdogOptions<T> {
  /** Sends the navigation (again, on a later try) and watches for its response head. */
  start(): Attempt<T>;
  /** What the load was, for the messages. */
  what: string;
  /** What the backend answers to a plain request, read when a try got no answer. */
  evidence(): Promise<string>;
  /** Where the line about a retried try goes. */
  warn(line: string): void;
  /** How many times the navigation is sent in all; 2 by default. */
  tries?: number;
}

/**
 * Runs the navigation and sends it again when its request is not answered in time. A navigation
 * that was answered is never cut short, however long it then takes to load, and one that fails by
 * itself (a refused connection, a bad address) fails with its own error.
 */
export async function navigateWithWatchdog<T>(
  options: WatchdogOptions<T>,
): Promise<T> {
  const tries = options.tries ?? 2;
  for (let attempt = 1; ; attempt += 1) {
    const { settled, answered } = options.start();
    // The send that a later try replaces ends with an abort nobody reads.
    settled.catch(() => undefined);
    const outcome = await Promise.race([
      settled.then(
        () => "settled" as const,
        () => "settled" as const,
      ),
      answered.then((ok) => (ok ? ("answered" as const) : ("silent" as const))),
    ]);
    if (outcome !== "silent") return settled;
    const evidence = await options.evidence();
    if (attempt >= tries) {
      throw new Error(
        `${options.what} got no response in ${String(tries)} tries (${evidence})`,
      );
    }
    options.warn(
      `${options.what} got no response, sending it again (try ${String(attempt + 1)} of ${String(tries)}; ${evidence})`,
    );
  }
}
