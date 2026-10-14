// SPDX-License-Identifier: GPL-3.0-or-later
// The inbox with 500 open requests (the fixture's `spread` step opens them): it must show them
// at once.
import type { Page } from "@playwright/test";
import { expect, test, type Backend } from "./fixture";

/** Data arrival to first paint, and to the last row painted, in the page just loaded. */
async function timings(page: Page) {
  // The rows are drawn in slices, and a host with every core busy takes seconds over them: wait for
  // the state (all rows, their marks) as long as it takes; the time it took is what the bars below judge.
  await expect(page.locator("li.req")).toHaveCount(501, { timeout: 30_000 });
  return page.evaluate(async () => {
    const at = (name: string) =>
      performance.getEntriesByName(name)[0]?.startTime ?? -1;
    for (let i = 0; i < 1500 && at("puddle:inbox-all-painted") < 0; i += 1) {
      await new Promise((r) => setTimeout(r, 20));
    }
    const data = at("puddle:inbox-data");
    return {
      first: at("puddle:inbox-painted") - data,
      all: at("puddle:inbox-all-painted") - data,
    };
  });
}

/** The inbox with 500 open requests, signed in. */
async function openSpread({ backend, page }: { backend: Backend; page: Page }) {
  await backend.control.step({ do: "spread", count: 500 });
  await backend.signIn(page);
}

// Wall-clock time on a busy machine (CI runners, a machine mid-build) is noisy: the best of five
// loads still reached 242 ms at a load average of 30. So the bars judge the best of five loads,
// and each load is its own test: five loads in one test ran past its 30 s on a host at load 300.
// The loads run in order in one worker and the last test judges what they measured; a real
// regression costs seconds in every load. The 200 ms first-paint target is 'under 200 ms on an
// idle machine'. The sliced drawing that makes it possible is asserted without a clock in
// src/routes/inbox/inbox.test.ts.
const FIRST_PAINT_TARGET_MS = 200;
const LOADS = 5;
test.describe("500 open requests", () => {
  test.describe.configure({ mode: "serial" });
  const runs: Awaited<ReturnType<typeof timings>>[] = [];

  test.describe("loads", () => {
    test.beforeEach(openSpread);
    for (let load = 1; load <= LOADS; load += 1) {
      test(`load ${load} of ${LOADS}: every row is drawn and the paints are marked`, async ({
        page,
      }) => {
        await page.goto("/inbox");
        const run = await timings(page);
        expect(run.first, "first paint was marked").toBeGreaterThan(0);
        expect(
          run.all,
          "the last paint was marked after the first",
        ).toBeGreaterThanOrEqual(run.first);
        runs.push(run);
      });
    }
  });

  test("the best load paints the first rows soon after their data arrives, all of them shortly after", async () => {
    expect(runs, "every load ran").toHaveLength(LOADS);
    const first = Math.min(...runs.map((r) => r.first));
    const all = Math.min(...runs.map((r) => r.all));
    test.info().annotations.push({
      type: "inbox-500",
      description: `best of ${LOADS}: first paint ${first.toFixed(0)} ms, all rows ${all.toFixed(0)} ms (runs: ${runs.map((r) => r.first.toFixed(0)).join(", ")})`,
    });
    expect(first, "data to first paint").toBeLessThan(
      FIRST_PAINT_TARGET_MS * 3,
    );
    expect(all, "data to all 501 rows painted").toBeLessThan(2000);
  });
});

test("a 500-row inbox is still driven by the keyboard", async ({
  page,
  request,
  backend,
}) => {
  await openSpread({ backend, page });
  // The J and K keys are a Settings switch, off by default.
  const reply = await request.put("/api/settings", {
    headers: { Authorization: `Bearer ${backend.token}` },
    data: { ui: { inbox_shortcuts: true } },
  });
  expect(reply.ok()).toBe(true);
  await page.goto("/inbox");
  await expect(page.locator("li.req")).toHaveCount(501, { timeout: 30_000 });
  await page.locator("main").focus();
  await page.keyboard.press("j");
  await page.keyboard.press("j");
  await expect(page.locator("li.req[aria-current='true']")).toBeFocused();
});
