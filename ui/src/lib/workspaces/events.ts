// SPDX-License-Identifier: AGPL-3.0-or-later
// The stream events the workspace screens act on, narrowed from the generated `Event`.
import type { components } from "#lib/api/schema.d.ts";

type Event = components["schemas"]["Event"];

export type StatusChanged = Extract<Event, { type: "status_changed" }>;
export type OomKill = Extract<Event, { type: "oom_kill" }>;
export type DiskNearlyFull = Extract<Event, { type: "disk_nearly_full" }>;
export type WorkspaceProgress = Extract<Event, { type: "workspace_progress" }>;
export type RepositoriesChanged = Extract<
  Event,
  { type: "workspace_repositories_changed" }
>;
export type WorkspaceEvent =
  | StatusChanged
  | OomKill
  | DiskNearlyFull
  | WorkspaceProgress
  | RepositoriesChanged;

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null;

/** The event if it is one the workspace screens use and its fields have the right types. */
export function asWorkspaceEvent(raw: unknown): WorkspaceEvent | null {
  if (!isObject(raw) || typeof raw["workspace"] !== "string") return null;
  switch (raw["type"]) {
    case "status_changed":
      return typeof raw["status"] === "string" ? (raw as StatusChanged) : null;
    case "oom_kill":
      return typeof raw["process"] === "string" &&
        typeof raw["pid"] === "number"
        ? (raw as OomKill)
        : null;
    case "disk_nearly_full":
      return typeof raw["percent"] === "number" &&
        (raw["disk"] === "root" || raw["disk"] === "volume")
        ? (raw as DiskNearlyFull)
        : null;
    case "workspace_progress":
      return typeof raw["step"] === "string"
        ? (raw as WorkspaceProgress)
        : null;
    case "workspace_repositories_changed":
      return raw as RepositoriesChanged;
    default:
      return null;
  }
}
