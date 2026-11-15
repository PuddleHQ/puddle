// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { loadFactor, MAX_LOAD_FACTOR } from "./load-budget.ts";

describe("loadFactor", () => {
  it("keeps the plain limit while there is at most one runnable task per core", () => {
    expect(loadFactor(0, 16)).toBe(1);
    expect(loadFactor(3.75, 40)).toBe(1);
    expect(loadFactor(16, 16)).toBe(1);
  });

  it("grows by one for each further core's worth of load", () => {
    expect(loadFactor(16.1, 16)).toBe(2);
    expect(loadFactor(32, 16)).toBe(2);
    expect(loadFactor(40, 16)).toBe(MAX_LOAD_FACTOR - 1);
  });

  it("stops at the cap, so a hang still fails", () => {
    expect(loadFactor(600, 16)).toBe(MAX_LOAD_FACTOR);
    expect(loadFactor(335, 40)).toBe(MAX_LOAD_FACTOR);
  });

  it("gives 1 for a reading that is not a number or a machine with no cores", () => {
    expect(loadFactor(Number.NaN, 16)).toBe(1);
    expect(loadFactor(Infinity, 16)).toBe(1);
    expect(loadFactor(20, 0)).toBe(1);
  });
});
