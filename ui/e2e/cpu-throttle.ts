// SPDX-License-Identifier: GPL-3.0-or-later
// The nightly stress job (ci/stress.sh) sets PUDDLE_E2E_CPU_THROTTLE to a slowdown factor (4 = a
// quarter of the speed) so that Chromium runs the page on a starved CPU. Unset, this does nothing.
// WebKit has no such protocol: the job starves its process with taskset and busy loops instead.
import type { Page } from "@playwright/test";

/** The slowdown the job asked for: 1 when unset or not above 1. */
export function slowdown(): number {
  const rate = Number(process.env["PUDDLE_E2E_CPU_THROTTLE"]);
  return rate > 1 ? rate : 1;
}

export async function throttleCpu(
  page: Page,
  browserName: string,
): Promise<void> {
  const rate = slowdown();
  if (browserName !== "chromium" || rate === 1) return;
  const session = await page.context().newCDPSession(page);
  await session.send("Emulation.setCPUThrottlingRate", { rate });
}
