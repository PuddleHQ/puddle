// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { startLagProbe } from "./lag-probe.ts";

function blockFor(ms: number): void {
  const end = performance.now() + ms;
  while (performance.now() < end) {
    // busy wait: the event loop cannot run any timer meanwhile
  }
}

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

describe("startLagProbe", () => {
  it("reports one line naming the worker and the file when the loop is blocked past the threshold", async () => {
    const lines: string[] = [];
    const stop = startLagProbe({
      thresholdMs: 1000,
      intervalMs: 100,
      write: (line) => lines.push(line),
      describe: () => "worker 7 src/x.test.ts",
    });
    await sleep(150);
    blockFor(1200);
    await sleep(300);
    stop();

    expect(lines).toHaveLength(1);
    const match =
      /^event-loop lag: worker 7 src\/x\.test\.ts lag (\d+) ms\n$/.exec(
        lines[0]!,
      );
    expect(match).not.toBeNull();
    // the last tick ran at most one interval before the block began
    expect(Number(match![1])).toBeGreaterThanOrEqual(1100);
  });

  it("stays silent when the loop is free and after it is stopped", async () => {
    const lines: string[] = [];
    const stop = startLagProbe({
      thresholdMs: 1000,
      intervalMs: 20,
      write: (line) => lines.push(line),
      describe: () => "w",
    });
    await sleep(200);
    stop();
    blockFor(1100);
    await sleep(100);
    expect(lines).toEqual([]);
  });
});
