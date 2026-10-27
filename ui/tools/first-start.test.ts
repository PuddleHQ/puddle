// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { PhaseTimer } from "./first-start.ts";

describe("PhaseTimer", () => {
  it("lists each phase with its duration in the order they ended", async () => {
    let clock = 0;
    const timer = new PhaseTimer(() => clock);
    expect(timer.describe()).toBe("");
    await timer.run("launch", () => {
      clock += 350;
      return Promise.resolve();
    });
    await expect(
      timer.run("first page", () => {
        clock += 2100;
        return Promise.resolve("page");
      }),
    ).resolves.toBe("page");
    expect(timer.describe()).toBe("launch 350 ms, first page 2100 ms");
  });

  it("records a phase that fails and passes the error on", async () => {
    let clock = 0;
    const timer = new PhaseTimer(() => clock);
    await expect(
      timer.run("first page", () => {
        clock += 45_000;
        return Promise.reject(new Error("took over 45000 ms"));
      }),
    ).rejects.toThrow("took over 45000 ms");
    expect(timer.describe()).toBe("first page 45000 ms");
  });
});
