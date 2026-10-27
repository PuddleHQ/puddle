// SPDX-License-Identifier: GPL-3.0-or-later

/**
 * Times the named phases of one start. A start that stalls or just runs long then says where the
 * time went ("launch 350 ms, first page 2100 ms") instead of only how long it took.
 */
export class PhaseTimer {
  private readonly spent: [string, number][] = [];
  private readonly now: () => number;

  constructor(now: () => number = Date.now) {
    this.now = now;
  }

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
