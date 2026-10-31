// SPDX-License-Identifier: GPL-3.0-or-later
// The live event the method and path section listens to. An event that is not an object with that
// exact type is ignored.
import type { components } from "#lib/api/schema.d.ts";

type Event = components["schemas"]["Event"];

/** Some rule on method and path was added or deleted. */
export type RequestRulesChanged = Extract<
  Event,
  { type: "request_rules_changed" }
>;

export function isRequestRulesChanged(
  value: unknown,
): value is RequestRulesChanged {
  return (
    typeof value === "object" &&
    value !== null &&
    (value as Record<string, unknown>)["type"] === "request_rules_changed"
  );
}
