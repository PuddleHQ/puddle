// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { formatNames, parseNames, sameNames } from "./names.ts";

describe("parseNames", () => {
  it("splits on lines, commas, semicolons and spaces and lower-cases", () => {
    expect(
      parseNames("Bank.example.com,\n*.Corp.example  intranet.test;x.io"),
    ).toEqual(["bank.example.com", "*.corp.example", "intranet.test", "x.io"]);
  });
  it("drops empties and repeats, keeping the first place", () => {
    expect(parseNames("  \n a.io\nA.IO\n,,b.io")).toEqual(["a.io", "b.io"]);
    expect(parseNames("")).toEqual([]);
  });
});

describe("formatNames and sameNames", () => {
  it("puts one name on each line, and the text parses back", () => {
    expect(formatNames(["a.io", "*.b.io"])).toBe("a.io\n*.b.io");
    expect(parseNames(formatNames(["a.io", "*.b.io"]))).toEqual([
      "a.io",
      "*.b.io",
    ]);
    expect(formatNames([])).toBe("");
  });
  it("compares as sets, ignoring order", () => {
    expect(sameNames(["b", "a"], ["a", "b"])).toBe(true);
    expect(sameNames(["a"], ["a", "b"])).toBe(false);
    expect(sameNames(["a", "b"], ["a", "c"])).toBe(false);
  });
});
