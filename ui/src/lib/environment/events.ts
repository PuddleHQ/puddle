// SPDX-License-Identifier: GPL-3.0-or-later
// The stream events the environment screens act on.
import type { components } from "#lib/api/schema.d.ts";

type Event = components["schemas"]["Event"];

export type EnvFailed = Extract<Event, { type: "workspace_env_failed" }>;

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null;

/**
 * Whether `event` changes the list a scope shows: a global change always does (a workspace's list
 * has the global entries too), a workspace change for that workspace only.
 */
export function isEnvChanged(
  event: unknown,
  scope: { kind: "global" } | { kind: "workspace"; name: string },
): boolean {
  if (!isObject(event)) return false;
  if (event["type"] === "global_env_changed") return true;
  return (
    scope.kind === "workspace" &&
    event["type"] === "workspace_env_changed" &&
    event["workspace"] === scope.name
  );
}

/** A `workspace_env_failed` event with the fields it must have, or `null`. */
export function asEnvFailed(event: unknown): EnvFailed | null {
  if (
    isObject(event) &&
    event["type"] === "workspace_env_failed" &&
    typeof event["workspace"] === "string" &&
    typeof event["reason"] === "string"
  ) {
    return event as EnvFailed;
  }
  return null;
}
