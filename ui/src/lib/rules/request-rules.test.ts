// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { requestRule } from "#lib/testing/fake-request-rules.ts";
import { isRequestRulesChanged } from "./request-rule-events.ts";
import {
  hostProblem,
  methodsLabel,
  methodsProblem,
  pathProblem,
  refusalField,
  requestRuleName,
  requestRuleWorkspaces,
  requestRulesView,
  scopeLabel,
  splitMethods,
} from "./request-rules.ts";

describe("how a rule reads", () => {
  it("names the methods, or every method", () => {
    expect(methodsLabel(requestRule(1))).toBe("Every method");
    expect(methodsLabel(requestRule(1, { methods: ["DELETE", "PUT"] }))).toBe(
      "DELETE, PUT",
    );
  });
  it("names where it applies", () => {
    expect(scopeLabel(requestRule(1))).toBe("Every workspace");
    expect(scopeLabel(requestRule(1, { workspace: "shop" as never }))).toBe(
      "shop",
    );
  });
  it("is named in full for buttons and messages", () => {
    expect(
      requestRuleName(
        requestRule(1, {
          host: "api.example.com",
          methods: ["DELETE"],
          path: "/repos/**",
        }),
      ),
    ).toBe("refuse DELETE /repos/** on api.example.com for every workspace");
    expect(
      requestRuleName(
        requestRule(1, { host: "a.example", workspace: "shop" as never }),
      ),
    ).toBe("refuse every method /** on a.example for workspace shop");
  });
});

describe("which rules show", () => {
  const rules = [
    requestRule(1, { host: "b.example", path: "/z" }),
    requestRule(2, {
      host: "a.example",
      path: "/z",
      workspace: "shop" as never,
    }),
    requestRule(3, { host: "a.example", path: "/a" }),
    requestRule(4, {
      host: "a.example",
      path: "/a",
      workspace: "docs" as never,
    }),
  ];
  it("sorts by host, then path, then id", () => {
    expect(requestRulesView(rules).map((r) => r.id)).toEqual([3, 4, 2, 1]);
  });
  it("keeps the global rules and one workspace's own", () => {
    expect(requestRulesView(rules, "shop").map((r) => r.id)).toEqual([3, 2, 1]);
    expect(requestRulesView(rules, "none").map((r) => r.id)).toEqual([3, 1]);
  });
  it("lists the workspaces the rules name once, in order", () => {
    expect(requestRuleWorkspaces(rules)).toEqual(["docs", "shop"]);
  });
});

describe("the host check", () => {
  it.each([
    ["api.example.com", null],
    ["*.example.com", null],
    ["  api.example.com  ", null],
  ])("accepts %s", (host, problem) => expect(hostProblem(host)).toBe(problem));
  it.each([
    ["", /Enter a host/],
    ["   ", /Enter a host/],
    ["https://api.example.com", /host name only/],
    ["api.example.com:8443", /host name only/],
    ["api.example.com/repos", /host name only/],
    ["a*.example.com", /host name only/],
    ["*.*.example.com", /host name only/],
    ["10.0.0.1", /IP addresses/],
    ["*.com", /two labels/],
  ])("refuses %j", (host, words) => expect(hostProblem(host)).toMatch(words));
});

describe("the methods check", () => {
  it("splits at commas and spaces, upper-cases and drops repeats", () => {
    expect(splitMethods("delete, Put  delete,,")).toEqual(["DELETE", "PUT"]);
    expect(splitMethods("")).toEqual([]);
  });
  it("accepts none, or HTTP tokens", () => {
    expect(methodsProblem("")).toBeNull();
    expect(methodsProblem("get, post, M-SEARCH")).toBeNull();
  });
  it("refuses a name that is not an HTTP token", () => {
    expect(methodsProblem("GE(T)")).toMatch(/"GE\(T\)" is not an HTTP method/);
    expect(methodsProblem("A".repeat(40))).toMatch(/is not an HTTP method/);
  });
  it("refuses more than 16 methods", () => {
    const many = Array.from({ length: 17 }, (_, i) => `M${i}`).join(",");
    expect(methodsProblem(many)).toMatch(/at most 16/);
  });
});

describe("the path check", () => {
  it.each([
    "/",
    "/**",
    "/admin",
    "/repos/*/issues",
    "/gists/**",
    "/a//b",
    "/a/**/",
  ])("accepts %s", (path) => expect(pathProblem(path)).toBeNull());
  it.each([
    ["", /Enter a path/],
    ["admin", /starts with \//],
    ["/a\u0000b", /control characters/],
    ["/a\nb", /control characters/],
    [`/${"a".repeat(512)}`, /at most 512 bytes/],
    [`/${"é".repeat(300)}`, /at most 512 bytes/],
    ["/a?x=1", /no query string/],
    ["/a#frag", /no query string/],
    ["/a*", /whole segment/],
    ["/*b/c", /whole segment/],
    ["/**/a", /only end a path/],
    [
      `/${Array.from({ length: 33 }, () => "a").join("/")}`,
      /at most 32 segments/,
    ],
  ])("refuses %j", (path, words) => expect(pathProblem(path)).toMatch(words));
  it("counts the segments the API counts: not a closing ** and not . or ..", () => {
    const names = Array.from({ length: 32 }, () => "a").join("/");
    expect(pathProblem(`/${names}/**`)).toBeNull();
    // A path with `.` or `..` is counted by the API after it resolves them.
    expect(pathProblem(`/./${names}/x`)).toBeNull();
  });
  it("allows exactly 32 segments", () => {
    expect(
      pathProblem(`/${Array.from({ length: 32 }, () => "a").join("/")}`),
    ).toBeNull();
  });
});

describe("where a refusal belongs", () => {
  it("reads the prefix the API puts on methods, path and workspace", () => {
    expect(refusalField("methods: GE T is not a method")).toEqual({
      field: "methods",
      message: "GE T is not a method",
    });
    expect(refusalField("Path: no good").field).toBe("path");
    expect(refusalField("workspace: unknown").field).toBe("workspace");
  });
  it("keeps the per-scope limit for the form", () => {
    expect(refusalField("At most 500 rules in one scope.").field).toBe("form");
  });
  it("takes any other refusal for the host", () => {
    expect(refusalField('"x" is not a host name.')).toEqual({
      field: "host",
      message: '"x" is not a host name.',
    });
  });
});

describe("the live event", () => {
  it("is exactly request_rules_changed", () => {
    expect(isRequestRulesChanged({ type: "request_rules_changed" })).toBe(true);
    expect(isRequestRulesChanged({ type: "rules_changed" })).toBe(false);
    expect(isRequestRulesChanged(null)).toBe(false);
    expect(isRequestRulesChanged("request_rules_changed")).toBe(false);
  });
});
