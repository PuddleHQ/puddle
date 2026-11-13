// SPDX-License-Identifier: AGPL-3.0-or-later
import { performance } from "node:perf_hooks";
import { setInterval, clearInterval } from "node:timers";

export interface LagProbeOptions {
  /** Lag in ms above which one line is written. */
  thresholdMs: number;
  /** Sampling period in ms. */
  intervalMs: number;
  /** Where the line goes (stderr in the test setup). */
  write: (line: string) => void;
  /** Names the worker and the running test file in the line. */
  describe: () => string;
}

/**
 * Samples how late a repeating timer fires, i.e. how long this thread's event loop was
 * stuck, and writes one line per stall over the threshold. Starting it changes no test
 * result: the timer is unref'd and only reads the clock. Returns a function that stops it.
 */
export function startLagProbe(options: LagProbeOptions): () => void {
  let last = performance.now();
  const timer = setInterval(() => {
    const now = performance.now();
    const lagMs = Math.round(now - last - options.intervalMs);
    last = now;
    if (lagMs > options.thresholdMs) {
      options.write(`event-loop lag: ${options.describe()} lag ${lagMs} ms\n`);
    }
  }, options.intervalMs);
  timer.unref();
  return () => clearInterval(timer);
}
