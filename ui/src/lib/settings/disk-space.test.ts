// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  asDiskSpace,
  gbFromMib,
  linesProblem,
  parseFreeGb,
} from "./disk-space.ts";

describe("parseFreeGb", () => {
  it("takes whole numbers from 1 to 10000 and says what is wrong otherwise", () => {
    expect(parseFreeGb(" 20 ")).toEqual({ ok: true, gb: 20 });
    expect(parseFreeGb("1")).toEqual({ ok: true, gb: 1 });
    expect(parseFreeGb("10000")).toEqual({ ok: true, gb: 10_000 });
    expect(parseFreeGb("0")).toEqual({
      ok: false,
      message: "Use between 1 and 10000 GB.",
    });
    expect(parseFreeGb("10001")).toMatchObject({ ok: false });
    for (const bad of ["", "abc", "2.5", "-3", "1e3"]) {
      expect(parseFreeGb(bad)).toEqual({
        ok: false,
        message: "Enter a whole number of GB.",
      });
    }
  });
});

describe("linesProblem", () => {
  it("refuses a failure line above the warning line and allows equal lines", () => {
    expect(linesProblem(20, 5)).toBeNull();
    expect(linesProblem(5, 5)).toBeNull();
    expect(linesProblem(5, 6)).toBe(
      "The failure line (6 GB) cannot be above the warning line (5 GB).",
    );
  });
});

describe("gbFromMib", () => {
  it("shows a decimal only when the amount is not whole", () => {
    expect(gbFromMib(15_360)).toBe("15");
    expect(gbFromMib(3_584)).toBe("3.5");
    expect(gbFromMib(0)).toBe("0");
  });
});

describe("asDiskSpace", () => {
  it("reads the event and nothing else", () => {
    expect(
      asDiskSpace({ type: "disk_space_changed", level: "low", free_mib: 9 }),
    ).toEqual({ level: "low", free_mib: 9 });
    expect(asDiskSpace({ type: "oom_kill" })).toBeNull();
    expect(
      asDiskSpace({ type: "disk_space_changed", level: "bad", free_mib: 9 }),
    ).toBeNull();
    expect(
      asDiskSpace({ type: "disk_space_changed", level: "ok", free_mib: "9" }),
    ).toBeNull();
    expect(asDiskSpace(null)).toBeNull();
    expect(asDiskSpace("disk_space_changed")).toBeNull();
  });
});
