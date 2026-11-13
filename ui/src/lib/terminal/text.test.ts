// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { BROKE, cleanText, cleanTitle, endLine } from "./text.ts";

const RLO = String.fromCodePoint(0x202e);
const ZWSP = String.fromCodePoint(0x200b);
const LRI = String.fromCodePoint(0x2066);
const BOM = String.fromCodePoint(0xfeff);
const ALM = String.fromCodePoint(0x061c);

describe("cleanText", () => {
  it("drops control, bidi and zero-width characters", () => {
    expect(cleanText(`a\u001b[31mb${RLO}c${ZWSP}d${LRI}e${BOM}f${ALM}g`)).toBe(
      "a[31mbcdefg",
    );
    expect(cleanText("plain text ü 日本")).toBe("plain text ü 日本");
  });
});

describe("cleanTitle", () => {
  it("cuts at 40 characters, counting a code point once", () => {
    expect(cleanTitle("x".repeat(60))).toBe("x".repeat(40));
    expect(cleanTitle("🦆".repeat(50))).toBe("🦆".repeat(40));
  });
  it("trims and cleans, and says nothing when nothing is left", () => {
    expect(cleanTitle(`  vim ${RLO} notes \n`)).toBe("vim  notes");
    expect(cleanTitle(`${ZWSP}${RLO}`)).toBe("");
  });
});

describe("endLine", () => {
  it("says why the shell ended", () => {
    expect(endLine({ reason: "exited", status: 2, message: null })).toEqual({
      line: "The shell exited with status 2.",
      detail: null,
      action: "restart",
    });
    expect(
      endLine({ reason: "exited", status: null, message: null }).line,
    ).toBe("The shell exited.");
  });
  it("offers Start when the workspace stopped or crashed", () => {
    expect(endLine({ reason: "stopped", status: null, message: null })).toEqual(
      {
        line: "The workspace stopped; the terminal ended.",
        detail: null,
        action: "start",
      },
    );
    expect(
      endLine({
        reason: "crashed",
        status: null,
        message: `killed${RLO} by oom`,
      }),
    ).toEqual({
      line: "The workspace crashed; the terminal ended.",
      detail: "killed by oom",
      action: "start",
    });
    expect(
      endLine({ reason: "crashed", status: null, message: null }).detail,
    ).toBeNull();
  });
  it("shows the boot's reason and points at the Overview", () => {
    expect(
      endLine({ reason: "boot_failed", status: null, message: "hook failed" }),
    ).toEqual({ line: "hook failed", detail: null, action: "overview" });
    expect(
      endLine({ reason: "boot_failed", status: null, message: null }).line,
    ).toBe("The workspace did not finish starting.");
  });
  it("calls a quitting Puddle a broken connection", () => {
    expect(endLine({ reason: "quitting", status: null, message: null })).toBe(
      BROKE,
    );
    expect(BROKE.action).toBe("new");
  });
});
