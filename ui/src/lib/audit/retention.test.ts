// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  growthLabel,
  isLarge,
  largeWarning,
  oldestLabel,
  parseDays,
  perDayLabel,
  recordsLabel,
  type AuditSize,
} from "./retention.ts";

const size: AuditSize = {
  records: 12,
  bytes: 3_000,
  oldest_ts: Date.UTC(2026, 10, 10),
  records_last_day: 1,
  bytes_last_day: 500,
  records_last_week: 7,
  bytes_last_week: 7_168,
  retention_days: 90,
  warn_bytes: 3_000,
  file_bytes: 4_000,
  free_bytes: 1_000,
};

describe("the activity log's size", () => {
  it("is large only when the file in use is above the warning size, not at it", () => {
    expect(isLarge(size)).toBe(false);
    expect(largeWarning(size)).toBeNull();
    const big = { ...size, free_bytes: 999 };
    expect(isLarge(big)).toBe(true);
    expect(largeWarning(big)).toBe(
      "The activity log is getting large: its file holds 2.9 KB, and Puddle warns at 2.9 KB. Keep it for fewer days to make it smaller.",
    );
  });

  it("counts records and says the growth per day", () => {
    expect(recordsLabel(1)).toBe("1 record");
    expect(recordsLabel(1200)).toBe("1,200 records");
    expect(growthLabel(7, 7_168)).toBe("7 records, 7.0 KB");
    expect(perDayLabel(size)).toBe("1.0 KB a day");
  });

  it("says how old the oldest record is, or that there is none", () => {
    const day = Date.UTC(2026, 10, 10);
    expect(oldestLabel(size, day + 3 * 86_400_000)).toBe(
      "2026-11-10 (3 days ago)",
    );
    expect(oldestLabel(size, day + 86_400_000)).toBe("2026-11-10 (1 day ago)");
    expect(oldestLabel(size, day + 1000)).toBe("2026-11-10 (today)");
    expect(oldestLabel(size, day - 5000)).toBe("2026-11-10 (today)");
    expect(oldestLabel({ ...size, oldest_ts: null }, day)).toBe(
      "No records yet",
    );
  });
});

describe("the days typed in", () => {
  it("accepts a whole number, trimmed, and leaves the range to the server", () => {
    expect(parseDays(" 30 ")).toEqual({ ok: true, days: 30 });
    expect(parseDays("0")).toEqual({ ok: true, days: 0 });
    expect(parseDays("99999")).toEqual({ ok: true, days: 99999 });
  });

  it("refuses text, decimals and an empty box", () => {
    for (const text of ["", "  ", "abc", "1.5", "1e3", "9".repeat(30)]) {
      expect(parseDays(text)).toEqual({
        ok: false,
        message: "Type a whole number of days.",
      });
    }
  });
});
