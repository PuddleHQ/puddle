// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { firstStart, PhaseTimer } from "./first-start.ts";

describe("firstStart", () => {
  it("returns the first try's result without logging", async () => {
    const log = vi.fn();
    const attempt = vi.fn().mockResolvedValue(7);
    await expect(
      firstStart({ what: "the x browser", tries: 3, attempt, log }),
    ).resolves.toBe(7);
    expect(attempt).toHaveBeenCalledTimes(1);
    expect(log).not.toHaveBeenCalled();
  });

  it("tries again after a stalled try and says which try stalled", async () => {
    const log = vi.fn();
    const attempt = vi
      .fn()
      .mockRejectedValueOnce(new Error("opening the first page took over 5 ms"))
      .mockResolvedValue("open");
    await expect(
      firstStart({ what: "the x browser", tries: 3, attempt, log }),
    ).resolves.toBe("open");
    expect(attempt).toHaveBeenCalledTimes(2);
    expect(log).toHaveBeenCalledExactlyOnceWith(
      "the x browser: try 1 of 3 failed: opening the first page took over 5 ms",
    );
  });

  it("fails by naming the browser and the last reason when every try stalls", async () => {
    const log = vi.fn();
    const attempt = vi
      .fn()
      .mockRejectedValueOnce(new Error("first"))
      .mockRejectedValueOnce("second")
      .mockRejectedValueOnce(new Error("third"));
    await expect(
      firstStart({ what: "the x browser", tries: 3, attempt, log }),
    ).rejects.toThrow(
      "the x browser did not open the app in 3 tries; the last one failed with: third",
    );
    expect(attempt).toHaveBeenCalledTimes(3);
    expect(log).toHaveBeenCalledTimes(3);
  });
});

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
