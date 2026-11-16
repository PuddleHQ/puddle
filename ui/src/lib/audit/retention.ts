// SPDX-License-Identifier: AGPL-3.0-or-later
// What the activity log card says about the log's size and how long it is kept. Pure: nothing here
// touches the page or the network.
import type { components } from "#lib/api/schema.d.ts";
import { bytesLabel } from "./model.ts";

export type AuditSize = components["schemas"]["AuditSize"];

const DAY_MS = 86_400_000;

/** Whether the log's lines are over the size Puddle warns at. */
export function isLarge(size: AuditSize): boolean {
  return size.bytes > size.warn_bytes;
}

/** The warning above the numbers; nothing while the log is a normal size. */
export function largeWarning(size: AuditSize): string | null {
  return isLarge(size)
    ? `The activity log is getting large: ${bytesLabel(size.bytes)}, and Puddle warns at ${bytesLabel(size.warn_bytes)}. Keep it for fewer days to make it smaller.`
    : null;
}

/** A count with its unit, "1 record" or "2 records". */
export function recordsLabel(count: number): string {
  return `${count.toLocaleString("en")} ${count === 1 ? "record" : "records"}`;
}

/** Records and bytes together, "12 records, 3.4 KB". */
export function growthLabel(records: number, bytes: number): string {
  return `${recordsLabel(records)}, ${bytesLabel(bytes)}`;
}

/** The average weekly growth per day, from the last 7 days. */
export function perDayLabel(size: AuditSize): string {
  return `${bytesLabel(Math.round(size.bytes_last_week / 7))} a day`;
}

/** How old the oldest record is, "about 3 days", or why there is none. */
export function oldestLabel(size: AuditSize, now: number): string {
  if (size.oldest_ts === null) return "No records yet";
  const days = Math.floor(Math.max(0, now - size.oldest_ts) / DAY_MS);
  const date = new Date(size.oldest_ts).toISOString().slice(0, 10);
  if (days === 0) return `${date} (today)`;
  return `${date} (${days} ${days === 1 ? "day" : "days"} ago)`;
}

/** The days typed in the box as a number, or why it is not one; the server judges the range. */
export function parseDays(
  text: string,
): { ok: true; days: number } | { ok: false; message: string } {
  const trimmed = text.trim();
  if (!/^-?\d+$/.test(trimmed)) {
    return { ok: false, message: "Type a whole number of days." };
  }
  const days = Number(trimmed);
  if (!Number.isSafeInteger(days)) {
    return { ok: false, message: "Type a whole number of days." };
  }
  return { ok: true, days };
}
