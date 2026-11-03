// SPDX-License-Identifier: GPL-3.0-or-later
// The limits a test and its reads get: 30 s and 15 s, times the slowdown the run declares (1
// outside the stress job), because a starved page's own start is that much dearer.
import { expect, test } from "@playwright/test";
import config from "../playwright.config.ts";
import { slowdown } from "./cpu-throttle.ts";

test("a test's limit and a read's limit grow by the slowdown the run declares", () => {
  expect(test.info().timeout).toBe(30_000 * slowdown());
  expect(config.timeout).toBe(30_000 * slowdown());
  expect(config.expect?.timeout).toBe(15_000 * slowdown());
});
