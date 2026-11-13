// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { secret, variable } from "#lib/testing/fake-environment.ts";
import {
  counts,
  hostsSummary,
  hostsText,
  nameProblem,
  ordered,
  parseHosts,
  placeOf,
  placeSummary,
  plural,
} from "./model.ts";

describe("a variable name", () => {
  it.each(["MY_TOKEN", "_x", "a1", "Path2"])("accepts %s", (name) => {
    expect(nameProblem(name)).toBeNull();
  });

  it("says what is wrong with an empty name and an odd one", () => {
    expect(nameProblem("")).toBe("Give the variable a name.");
    for (const bad of ["1A", "A-B", "A B", "ä"]) {
      expect(nameProblem(bad)).toMatch(/letters, digits and _/);
    }
  });
});

describe("hosts", () => {
  it("splits on commas, spaces and lines, lowers the case and keeps each once", () => {
    expect(
      parseHosts("API.example.com, *.example.org\napi.example.com  "),
    ).toEqual(["api.example.com", "*.example.org"]);
    expect(parseHosts("  \n,")).toEqual([]);
  });

  it("shows one per line for editing and a short summary in a row", () => {
    expect(hostsText(["a.com", "b.com"])).toBe("a.com\nb.com");
    expect(hostsSummary(["a.com"])).toBe("a.com");
    expect(hostsSummary(["a.com", "b.com"])).toBe("a.com and b.com");
    expect(hostsSummary(["a.com", "b.com", "c.com", "d.com"])).toBe(
      "a.com, b.com and 2 more",
    );
  });
});

describe("a list", () => {
  it("puts the workspace's own first, each group by name", () => {
    const list = [
      variable("Z", { scope: "global" }),
      variable("B"),
      variable("A", { scope: "global" }),
      variable("C"),
    ];
    expect(ordered(list).map((v) => `${v.scope}:${v.name}`)).toEqual([
      "workspace:B",
      "workspace:C",
      "global:A",
      "global:Z",
    ]);
    expect(list.map((v) => v.name)).toEqual(["Z", "B", "A", "C"]);
  });

  it("counts variables and secrets apart", () => {
    expect(
      counts([variable("A"), secret("B", ["x.com"]), secret("C", ["y.com"])]),
    ).toEqual({ variables: 1, secrets: 2 });
    expect(plural(1, "secret")).toBe("1 secret");
    expect(plural(0, "secret")).toBe("0 secrets");
  });
});

describe("where a secret is swapped", () => {
  it("is a header or the path whatever the parameter name says", () => {
    expect(placeOf("header", "ignored")).toEqual({ in: "header" });
    expect(placeOf("path", "")).toEqual({ in: "path" });
  });

  it("names a query parameter by the service's rules", () => {
    expect(placeOf("query", " appid ")).toEqual({ in: "query", name: "appid" });
    expect(placeOf("query", "a[b]")).toEqual({ in: "query", name: "a[b]" });
    expect(placeOf("query", "n".repeat(64))).toEqual({
      in: "query",
      name: "n".repeat(64),
    });
    expect(placeOf("query", "  ")).toEqual({
      problem: "Name the query parameter the tool puts the secret in.",
    });
    for (const bad of [
      "a&b",
      "a=b",
      "a#",
      "a%41",
      "a+b",
      "a b",
      "é",
      "n".repeat(65),
    ]) {
      expect(placeOf("query", bad)).toEqual({
        problem: expect.stringContaining("without spaces or & = # % +"),
      });
    }
  });

  it("is said after a row's hosts, and nothing for header values", () => {
    expect(placeSummary({ in: "query", name: "appid" })).toBe(
      " in the query parameter appid",
    );
    expect(placeSummary({ in: "path" })).toBe(" in the path");
    expect(placeSummary({ in: "header" })).toBe("");
    expect(placeSummary(null)).toBe("");
  });
});
