// SPDX-License-Identifier: AGPL-3.0-or-later

/**
 * A test build's planted breakage: `VITE_PUDDLE_PLANT_ROUTE_THROW=activity` makes the Activity
 * screen throw when it renders. The msi workflow sets it for a by-hand run planting `route-throws`,
 * to show the upgrade job's screen check goes red on a screen that fails; no other build sets it.
 */
export function plantedRouteThrow(route: string): void {
  if (import.meta.env["VITE_PUDDLE_PLANT_ROUTE_THROW"] === route)
    throw new Error(`planted: the ${route} screen throws`);
}
