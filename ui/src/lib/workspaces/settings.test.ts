// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  ALLOW_OPTIONS,
  CLIPBOARD_OPTIONS,
  SWITCH_OPTIONS,
  cpuChoices,
  fromOnOff,
  memoryChoices,
  onOff,
  sourceLabel,
} from "./settings.ts";

describe("memory", () => {
  it("lists the sizes in order, none of them worded as a default", () => {
    expect(memoryChoices(8192).map((o) => o.label)).toEqual([
      "2 GiB",
      "4 GiB",
      "8 GiB",
      "12 GiB",
      "16 GiB",
      "24 GiB",
      "32 GiB",
    ]);
  });

  it("adds the size in effect when it is not a standard one, in its place", () => {
    const labels = memoryChoices(5120).map((o) => o.label);
    expect(labels.indexOf("5 GiB")).toBeGreaterThan(labels.indexOf("4 GiB"));
    expect(labels.indexOf("5 GiB")).toBeLessThan(labels.indexOf("8 GiB"));
    expect(memoryChoices(4096)).toHaveLength(7);
  });
});

describe("yes-or-no settings", () => {
  it("maps a value to its dropdown choice and back", () => {
    expect(onOff(true)).toBe("on");
    expect(onOff(false)).toBe("off");
    expect(fromOnOff("on")).toBe(true);
    expect(fromOnOff("off")).toBe(false);
  });

  it("shows only the values, with no option that says to follow the global setting", () => {
    expect(ALLOW_OPTIONS.map((o) => o.label)).toEqual([
      "Allowed",
      "Not allowed",
    ]);
    expect(SWITCH_OPTIONS.map((o) => o.label)).toEqual(["On", "Off"]);
    expect(CLIPBOARD_OPTIONS.map((o) => o.value)).toEqual([
      "ask",
      "allow",
      "deny",
    ]);
  });
});

describe("sources", () => {
  it("are said in words", () => {
    expect(sourceLabel("workspace")).toBe("This workspace");
    expect(sourceLabel("global")).toBe("Global setting");
    expect(sourceLabel("default")).toBe("Puddle's default");
  });
});

describe("cpus", () => {
  it("lists 1 up to the maximum", () => {
    const options = cpuChoices(32, 2);
    expect(options).toHaveLength(32);
    expect(options[0]).toEqual({ value: "1", label: "1 CPU" });
    expect(options[31]).toEqual({ value: "32", label: "32 CPUs" });
  });

  it("has no fixed cap: the maximum is the host's, and a count above it stays", () => {
    expect(cpuChoices(64, 4)).toHaveLength(64);
    expect(cpuChoices(2, 6).map((o) => o.value)).toEqual(["1", "2", "6"]);
  });
});
