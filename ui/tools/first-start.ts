// SPDX-License-Identifier: GPL-3.0-or-later

/** A stall is any failure of one try, including the try's own time limit running out. */
export interface FirstStart<T> {
  /** Names what is started, for the error that ends a run of failed tries ("the msedge browser"). */
  what: string;
  tries: number;
  /** One try. It must end by itself (its own time limit), or the next try never comes. */
  attempt: () => Promise<T>;
  log: (line: string) => void;
}

/**
 * Runs `attempt` until it succeeds, at most `tries` times. Every failed try is logged with its
 * reason; when all of them fail the error names `what` and the last reason, so a browser that
 * never starts fails the run and says why instead of being retried quietly.
 */
export async function firstStart<T>(start: FirstStart<T>): Promise<T> {
  let last = "";
  for (let n = 1; n <= start.tries; n += 1) {
    try {
      return await start.attempt();
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
      start.log(`${start.what}: try ${n} of ${start.tries} failed: ${last}`);
    }
  }
  throw new Error(
    `${start.what} did not open the app in ${start.tries} tries; the last one failed with: ${last}`,
  );
}

/**
 * Times the named phases of one try. A try that stalls or just runs long then says where the
 * time went ("launch 350 ms, first page 2100 ms") instead of only how long it took.
 */
export class PhaseTimer {
  private readonly spent: [string, number][] = [];

  constructor(private readonly now: () => number = Date.now) {}

  /** Runs `work` and records its duration, also when it fails. */
  async run<T>(phase: string, work: () => Promise<T>): Promise<T> {
    const began = this.now();
    try {
      return await work();
    } finally {
      this.spent.push([phase, this.now() - began]);
    }
  }

  /** "launch 350 ms, first page 2100 ms", in the order the phases ended; "" before the first. */
  describe(): string {
    return this.spent.map(([phase, ms]) => `${phase} ${ms} ms`).join(", ");
  }
}
