// SPDX-License-Identifier: AGPL-3.0-or-later
// A bounded poll for the Windows end-to-end scripts.
//
// A probe that throws counts as "not yet" (the page or the API is not up), except a [`GiveUp`]:
// that one ends the wait at once with its own message, so a crashed app or a vanished workspace
// reads as that, not as a timeout of minutes.

import { setTimeout as sleep } from "node:timers/promises";

/** Thrown by a probe when waiting longer cannot help. [`until`] rethrows it unchanged. */
export class GiveUp extends Error {
  constructor(message) {
    super(message);
    this.name = "GiveUp";
  }
}

/**
 * Polls `probe` until it returns something truthy, for `within` ms, every `every` ms. The reason
 * is named at each call, never a fixed sleep.
 */
export async function until(what, probe, within = 60_000, every = 500) {
  const deadline = Date.now() + within;
  for (;;) {
    const value = await Promise.resolve()
      .then(probe)
      .catch((err) => {
        if (err instanceof GiveUp) throw err;
        return undefined;
      });
    if (value) return value;
    if (Date.now() > deadline) throw new Error(`timed out after ${Math.round(within / 1000)} s waiting for ${what}`);
    await sleep(every);
  }
}
