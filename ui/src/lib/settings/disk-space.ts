// SPDX-License-Identifier: AGPL-3.0-or-later
// The two free-space lines on the Settings screen: what a typed amount may be, and the notice the
// app raises when the drive that holds Puddle's data runs low. The host measures; the lines and
// their defaults match the API's (`ui.disk_warn_free_gb`, `ui.disk_fail_free_gb`).
import type { components } from "#lib/api/schema.d.ts";

export type DiskSpaceLevel = components["schemas"]["DiskSpaceLevel"];

export const FREE_GB_MIN = 1;
export const FREE_GB_MAX = 10_000;
export const DEFAULT_WARN_FREE_GB = 20;
export const DEFAULT_FAIL_FREE_GB = 5;

/** A free-space line in GB from what was typed, or the reason it cannot be used. */
export function parseFreeGb(
  text: string,
): { ok: true; gb: number } | { ok: false; message: string } {
  const trimmed = text.trim();
  if (!/^\d+$/.test(trimmed))
    return { ok: false, message: "Enter a whole number of GB." };
  const gb = Number(trimmed);
  if (gb < FREE_GB_MIN || gb > FREE_GB_MAX)
    return {
      ok: false,
      message: `Use between ${FREE_GB_MIN} and ${FREE_GB_MAX} GB.`,
    };
  return { ok: true, gb };
}

/** Why two lines cannot be saved together, or null: the failure line may not be above the warning line. */
export function linesProblem(warn: number, fail: number): string | null {
  return fail > warn
    ? `The failure line (${fail} GB) cannot be above the warning line (${warn} GB).`
    : null;
}

/** GB with one decimal only when it is not a whole number, from MiB. */
export function gbFromMib(mib: number): string {
  const tenths = Math.floor((mib * 10) / 1024);
  return tenths % 10 === 0 ? String(tenths / 10) : (tenths / 10).toFixed(1);
}

export interface DiskSpaceEvent {
  level: DiskSpaceLevel;
  free_mib: number;
}

/** The `disk_space_changed` event, or null for anything else. */
export function asDiskSpace(raw: unknown): DiskSpaceEvent | null {
  if (typeof raw !== "object" || raw === null) return null;
  const e = raw as { type?: unknown; level?: unknown; free_mib?: unknown };
  if (e.type !== "disk_space_changed") return null;
  if (e.level !== "ok" && e.level !== "low" && e.level !== "critical")
    return null;
  if (typeof e.free_mib !== "number") return null;
  return { level: e.level, free_mib: e.free_mib };
}
