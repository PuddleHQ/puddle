// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  DEFAULT_RANGE,
  NO_FILTER,
  OUTCOMES,
  TYPES,
  fromSearch,
  isFiltered,
  isNarrowed,
  toQuery,
  toSearch,
  type Filter,
} from "./filter.ts";

describe("filters and the API query", () => {
  it("sends nothing for the defaults but the range", () => {
    expect(toQuery(NO_FILTER, 100_000_000)).toEqual({
      from: 100_000_000 - 86_400_000,
    });
    expect(toQuery({ ...NO_FILTER, range: "all" }, 5)).toEqual({});
  });
  it("sends every set filter, trimmed, and measures the range from now", () => {
    const filter: Filter = {
      workspace: "shop",
      type: "connection",
      outcome: "deny",
      category: "company_network",
      q: "  github  ",
      range: "1h",
    };
    expect(toQuery(filter, 10_000_000)).toEqual({
      workspace: "shop",
      type: "connection",
      outcome: "deny",
      category: "company_network",
      q: "github",
      from: 10_000_000 - 3_600_000,
    });
  });
  it("knows when a filter is not the default view", () => {
    expect(isFiltered(NO_FILTER)).toBe(false);
    expect(isFiltered({ ...NO_FILTER, q: "  " })).toBe(false);
    for (const change of [
      { workspace: "a" },
      { type: "connection" },
      { outcome: "allow" },
      { category: "company_network" },
      { q: "x" },
      { range: "7d" },
    ] as Partial<Filter>[]) {
      expect(isFiltered({ ...NO_FILTER, ...change })).toBe(true);
    }
  });
});

describe("isNarrowed", () => {
  it("is false only when nothing is left out of the log", () => {
    expect(isNarrowed({ ...NO_FILTER, range: "all" })).toBe(false);
    expect(isNarrowed(NO_FILTER)).toBe(true);
    expect(isNarrowed({ ...NO_FILTER, range: "all", q: "  " })).toBe(false);
    for (const change of [
      { workspace: "a" },
      { type: "connection" },
      { outcome: "allow" },
      { category: "company_network" },
      { q: "x" },
    ] as Partial<Filter>[]) {
      expect(isNarrowed({ ...NO_FILTER, range: "all", ...change })).toBe(true);
    }
  });
});

describe("the page address", () => {
  it("is empty for the default view", () => {
    expect(toSearch(NO_FILTER)).toBe("");
    expect(fromSearch("")).toEqual(NO_FILTER);
  });
  it("keeps the search in `q` and still reads the `host` of an older address", () => {
    expect(toSearch({ ...NO_FILTER, q: " /v1/items " })).toBe(
      "?q=%2Fv1%2Fitems",
    );
    expect(fromSearch("?q=post").q).toBe("post");
    expect(fromSearch("?host=github.com").q).toBe("github.com");
    expect(fromSearch("?q=a&host=b").q).toBe("a");
  });
  it("round-trips every filter, whatever the host holds", () => {
    const filter: Filter = {
      workspace: "shop",
      type: "rule_created",
      outcome: "blocked",
      category: "company_network",
      q: "a&b=c d",
      range: "all",
    };
    expect(fromSearch(toSearch(filter))).toEqual(filter);
    for (const t of TYPES)
      expect(fromSearch(toSearch({ ...NO_FILTER, type: t.value })).type).toBe(
        t.value,
      );
    for (const o of OUTCOMES)
      expect(
        fromSearch(toSearch({ ...NO_FILTER, outcome: o.value })).outcome,
      ).toBe(o.value);
  });
  it("drops what it does not recognise instead of failing", () => {
    expect(fromSearch("?type=nope&outcome=x&range=forever&other=1")).toEqual({
      ...NO_FILTER,
      range: DEFAULT_RANGE,
    });
  });
});
