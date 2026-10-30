// SPDX-License-Identifier: GPL-3.0-or-later
// The inbox with 500 open requests (the fixture's `spread` step opens them): it must show them
// at once.
import type { Page } from "@playwright/test";
import { slowdown } from "./cpu-throttle.ts";
import { expect, test, type Backend } from "./fixture";

// The bars below hold for the speed the target names. The nightly stress runs Chromium at a quarter
// of it (PUDDLE_E2E_CPU_THROTTLE) on two shared cores: the best of five loads then took 617 and
// 666 ms to the first paint and 2.6 s to the last (bars 600 ms and 2 s, 3 of 3 runs failed), so the
// bars grow by the slowdown that run declares, which is 1 everywhere else. The bars catch a
// regression, so they are not loosened for the sake of a machine: only for a slowdown asked for.
const SLOWDOWN = slowdown();
const ALL_ROWS_BAR_MS = 2000;
// A load also draws 501 rows on that slow page (7 to 20 s on a fast host under the stress, 0.6 s
// idle), so the 30 s a test gets and the wait for the rows grow by the same factor.
const TEST_TIMEOUT_MS = 30_000 * SLOWDOWN;
test.describe.configure({ timeout: TEST_TIMEOUT_MS });

/** Data arrival to first paint, and to the last row painted, in the page just loaded. */
async function timings(page: Page) {
  // The rows are drawn in slices, and a host with every core busy takes seconds over them: wait for
  // the state (all rows, their marks) as long as it takes; the time it took is what the bars below judge.
  await expect(page.locator("li.req")).toHaveCount(501, {
    timeout: TEST_TIMEOUT_MS,
  });
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
      FIRST_PAINT_TARGET_MS * 3 * SLOWDOWN,
    );
    expect(all, "data to all 501 rows painted").toBeLessThan(
      ALL_ROWS_BAR_MS * SLOWDOWN,
    );
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
  await expect(page.locator("li.req")).toHaveCount(501, {
    timeout: TEST_TIMEOUT_MS,
  });
  await page.locator("main").focus();
  await page.keyboard.press("j");
  await page.keyboard.press("j");
  await expect(page.locator("li.req[aria-current='true']")).toBeFocused();
});
