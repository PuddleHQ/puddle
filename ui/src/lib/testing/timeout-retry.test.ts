// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";

// A host stall can outlast a test's clock without the test being slow; the config gives a timed-out
// test one more attempt.
describe("a test that timed out", () => {
  it("gets a second attempt", { timeout: 50 }, async ({ task }) => {
    const attempt = task.result?.retryCount ?? 0;
    if (attempt === 0) await new Promise((r) => setTimeout(r, 200));
    expect(attempt).toBe(1);
  });
});
