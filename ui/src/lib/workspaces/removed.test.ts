// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import type { Rule } from "#lib/rules/model.ts";
import { FakeEnvApi, secret, variable } from "#lib/testing/fake-environment.ts";
import { readRemoved, removedText } from "./removed.ts";

const rule = (scope: Rule["scope"]): Rule => ({ scope }) as Rule;

describe("readRemoved", () => {
  it("counts the workspace's own variables, secrets and rules, not the global ones", async () => {
    const api = new FakeEnvApi();
    api.global = [variable("G", { scope: "global" })];
    api.own["web"] = [
      variable("A"),
      variable("B"),
      secret("S", ["a.example.com"]),
    ];
    api.rules = [
      rule({ type: "workspace", workspace: "web" }),
      rule({ type: "workspace", workspace: "docs" }),
      rule({ type: "global" }),
      rule({ type: "workspace", workspace: "web" }),
    ];
    expect(await readRemoved("web", api as never)).toEqual({
      variables: 2,
      secrets: 1,
      rules: 2,
    });
  });

  it("says nothing when a list can't be read", async () => {
    const api = new FakeEnvApi();
    api.down = true;
    expect(await readRemoved("web", api as never)).toBeNull();
    const refused = {
      GET: async () => ({ error: {}, response: { status: 500 } }),
    };
    expect(await readRemoved("web", refused as never)).toBeNull();
  });
});

describe("removedText", () => {
  it("names the three counts and leaves out what is zero", () => {
    expect(removedText({ variables: 2, secrets: 1, rules: 3 })).toBe(
      "2 variables, 1 secret and 3 rules",
    );
    expect(removedText({ variables: 0, secrets: 1, rules: 3 })).toBe(
      "1 secret and 3 rules",
    );
    expect(removedText({ variables: 0, secrets: 0, rules: 1 })).toBe("1 rule");
  });

  it("is empty for nothing and for an unknown count", () => {
    expect(removedText({ variables: 0, secrets: 0, rules: 0 })).toBeNull();
    expect(removedText(null)).toBeNull();
  });
});
