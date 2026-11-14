// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { GiveUp, until } from "../../crates/puddle-app/e2e/poll.mjs";

describe("until", () => {
  it("treats a throwing probe as not yet and returns the first truthy value", async () => {
    let calls = 0;
    const value = await until(
      "the third call",
      () => {
        calls += 1;
        if (calls < 3) throw new Error("not up yet");
        return "up";
      },
      5_000,
      1,
    );
    expect(value).toBe("up");
    expect(calls).toBe(3);
  });

  it("ends at once with the probe's own message when it gives up", async () => {
    const started = Date.now();
    await expect(
      until(
        "the workspace",
        () => {
          throw new GiveUp("the workspace vanished");
        },
        2_000,
        1,
      ),
    ).rejects.toThrow("the workspace vanished");
    expect(Date.now() - started).toBeLessThan(5_000);
  });

  it("names what it waited for when the time runs out", async () => {
    await expect(until("the page", () => false, 20, 1)).rejects.toThrow(/waiting for the page/);
  });
});
