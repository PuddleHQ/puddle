// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { firstStart } from "./first-start.ts";

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
