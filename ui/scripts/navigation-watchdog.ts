// SPDX-License-Identifier: AGPL-3.0-or-later
// A page load whose request gets no answer at all. The nightly stress run (WebKit on two starved
// cores) had `page.goto` sit on a request with no response for the whole 24 minutes of the test's
// limit (audit-retention, 2026-11-19: trace status -1, the same test and the same backend answered
// in 2 s on the other 2014 loads). A response head is the cheap part of a load, so waiting for it is
// not a speed limit like the load event: when it does not come, the navigation fails at once by name
// with what a plain fetch of the same address returned meanwhile, so the failure says whether the
// backend or the browser went quiet. It is not sent again: a resend would turn the stall into a
// green run and hide it.

export interface Attempt<T> {
  /** The navigation itself, as `page.goto` returns it. */
  settled: Promise<T>;
  /** Resolves true when the navigation's response head arrived, false when the limit passed without one. */
  answered: Promise<boolean>;
}

export interface WatchdogOptions<T> {
  /** Sends the navigation and watches for its response head. */
  start(): Attempt<T>;
  /** What the load was, for the message. */
  what: string;
  /** What the backend answers to a plain request, read when the navigation got no answer. */
  evidence(): Promise<string>;
}

/**
 * Runs the navigation and fails by name when its request is not answered in time. A navigation
 * that was answered is never cut short, however long it then takes to load, and one that fails by
 * itself (a refused connection, a bad address) fails with its own error.
 */
export async function navigateWithWatchdog<T>(
  options: WatchdogOptions<T>,
): Promise<T> {
  const { settled, answered } = options.start();
  // When the silence wins, the navigation's own late abort is nobody's to read.
  settled.catch(() => undefined);
  const outcome = await Promise.race([
    settled.then(
      () => "settled" as const,
      () => "settled" as const,
    ),
    answered.then((ok) => (ok ? ("answered" as const) : ("silent" as const))),
  ]);
  if (outcome === "silent") {
    throw new Error(
      `${options.what} got no response (${await options.evidence()})`,
    );
  }
  return settled;
}
