// SPDX-License-Identifier: AGPL-3.0-or-later
// "Restore from a file…" on the Settings screen: the file goes to the service, which checks it and
// answers what a restore would do; confirming asks the service to apply it at the next start. The
// safety copies of the state a restore replaced are listed here and removed one at a time.
import { api } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { readFailure, SERVICE_DOWN } from "#lib/stores/read-failure.ts";

export type RestorePreview = components["schemas"]["RestorePreview"];
export type RestorePart = components["schemas"]["RestorePart"];
export type RestoreMode = components["schemas"]["RestoreMode"];
export type RestoreResult = components["schemas"]["RestoreResult"];
export type SafetyCopy = components["schemas"]["SafetyCopy"];

export type Outcome<T> = { ok: true; value: T } | { ok: false; reason: string };

const down = (): { ok: false; reason: string } => ({
  ok: false,
  reason: SERVICE_DOWN,
});

/** Sends the file; nothing changes on this machine until the user confirms. */
export async function previewRestore(
  file: File,
): Promise<Outcome<RestorePreview>> {
  try {
    const { data, error, response } = await api.POST("/api/restore/preview", {
      // The file goes as it is; the typed body is a byte list, which the serializer skips.
      body: file as unknown as number[],
      bodySerializer: (body: unknown) => body as BodyInit,
      headers: { "Content-Type": "application/gzip" },
    });
    return data
      ? { ok: true, value: data }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return down();
  }
}

/** Confirms the restore of the staged copy; Puddle then restarts to apply it. */
export async function applyRestore(
  stagingId: string,
  mode: RestoreMode,
): Promise<Outcome<{ safetyCopy: string; restartRequired: boolean }>> {
  try {
    const { data, error, response } = await api.POST("/api/restore/apply", {
      body: { staging_id: stagingId, mode },
    });
    return data
      ? {
          ok: true,
          value: {
            safetyCopy: data.safety_copy,
            restartRequired: data.restart_required,
          },
        }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return down();
  }
}

/** What the last restore did, until the user has seen it; `null` when there is nothing. */
export async function loadResult(): Promise<Outcome<RestoreResult | null>> {
  try {
    const { data, error, response } = await api.GET("/api/restore/result");
    return data
      ? { ok: true, value: data.result }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return down();
  }
}

export async function dismissResult(): Promise<Outcome<null>> {
  try {
    const { error, response } = await api.DELETE("/api/restore/result");
    return response.ok
      ? { ok: true, value: null }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return down();
  }
}

export async function loadSafetyCopies(): Promise<Outcome<SafetyCopy[]>> {
  try {
    const { data, error, response } = await api.GET("/api/safety-copies");
    return data
      ? { ok: true, value: data.copies }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return down();
  }
}

export async function deleteSafetyCopy(name: string): Promise<Outcome<null>> {
  try {
    const { error, response } = await api.DELETE("/api/safety-copies/{name}", {
      params: { path: { name } },
    });
    return response.ok
      ? { ok: true, value: null }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return down();
  }
}

const PART_LABELS: Record<RestorePart["name"], string> = {
  rules: "Rules",
  rule_sets: "Rule sets",
  workspaces: "Workspaces",
  identities: "Git identities",
  environment: "Environment variables",
  settings: "Settings",
  users: "Users",
};

export function partLabel(name: RestorePart["name"]): string {
  return PART_LABELS[name];
}

/** What would be dropped by Replace, over every part that is not the users (they always stay). */
export function droppedByReplace(parts: RestorePart[]): number {
  return parts
    .filter((p) => p.name !== "users")
    .reduce((sum, p) => sum + p.only_here, 0);
}

/** What would be replaced by Replace and kept as it is by Add. */
export function differing(parts: RestorePart[]): number {
  return parts
    .filter((p) => p.name !== "users")
    .reduce((sum, p) => sum + p.differ, 0);
}

/** How many entries of the file this machine lacks. */
export function toAdd(parts: RestorePart[]): number {
  return parts.reduce((sum, p) => sum + p.add, 0);
}

/** `20261115T225500Z` as `2026-11-15 22:55 UTC`; any other name as it is. */
export function safetyCopyTime(name: string): string {
  const m = /^(\d{4})(\d{2})(\d{2})T(\d{2})(\d{2})\d{2}Z$/.exec(name);
  return m ? `${m[1]}-${m[2]}-${m[3]} ${m[4]}:${m[5]} UTC` : name;
}
