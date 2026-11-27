// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, describe, expect, it, vi } from "vitest";
import { plantedRouteThrow } from "./plant.ts";

describe("plantedRouteThrow", () => {
  afterEach(() => vi.unstubAllEnvs());

  it("does nothing in a build that plants no breakage", () => {
    expect(() => plantedRouteThrow("activity")).not.toThrow();
  });

  it("throws on the planted route only", () => {
    vi.stubEnv("VITE_PUDDLE_PLANT_ROUTE_THROW", "activity");
    expect(() => plantedRouteThrow("activity")).toThrow(
      "planted: the activity screen throws",
    );
    expect(() => plantedRouteThrow("rules")).not.toThrow();
  });
});
