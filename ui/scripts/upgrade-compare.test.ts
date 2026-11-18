// SPDX-License-Identifier: AGPL-3.0-or-later
// The breakages the upgrade's before-and-after comparison must catch (crates/puddle-app/e2e/upgrade-state.mjs):
// each case is a loss an upgrade could cause, and the comparison must name it; the last cases are what must
// not count as a loss.
import { describe, expect, it } from "vitest";
import {
  buildIsNotOlder,
  compareDump,
  compareDumps,
  compareVersions,
} from "../../crates/puddle-app/e2e/upgrade-compare.mjs";

const rules = {
  rules: [
    {
      id: 1,
      scope: { type: "global" },
      pattern: "kept-allow.example.net",
      effect: "allow",
      expires_at: null,
      precedence: 5,
    },
    {
      id: 2,
      scope: { type: "workspace", workspace: "w1" },
      pattern: "kept-deny.example.org",
      effect: "deny",
      expires_at: null,
      precedence: 5,
    },
  ],
};
const pending = {
  requests: [
    {
      id: 7,
      workspace: "w1",
      host: "example.com",
      port: 443,
      state: "requested",
      attempts: 1,
      last_seen: 10,
    },
  ],
};
const workspaces = {
  workspaces: [{ id: "w1", name: "upgrade-test", status: "running" }],
};
const identities = {
  identities: [
    {
      id: 3,
      label: "Kept identity",
      author: { name: "Kept", email: "kept@example.com" },
      credentials: [{ host: "github.com", last_check: { checked_at: 1 } }],
    },
  ],
};
const env = {
  variables: [
    { name: "QA_MARK", kind: "plain", value: "old-build", hosts: [] },
    { name: "QA_SECRET", kind: "secret", value: null, hosts: ["example.net"] },
  ],
};
const settings = {
  workspace_defaults: { memory: 3072 },
  vscode_server: {},
  ui: { theme: "dark", close_behaviour: "quit" },
  tools: {},
  limits: {},
  revision: "a",
  effective: { memory: 3072 },
};

function at<T>(list: T[], index: number): T {
  const item = list[index];
  if (item === undefined) throw new Error(`no entry ${index}`);
  return item;
}

const same = (path: string, body: unknown) =>
  compareDump(path, body, structuredClone(body));

describe("an upgrade that keeps everything", () => {
  it("compares equal for every dump", () => {
    for (const [path, body] of Object.entries({
      "/api/workspaces": workspaces,
      "/api/rules": rules,
      "/api/pending": pending,
      "/api/identities": identities,
      "/api/env": env,
      "/api/settings": settings,
    })) {
      expect(same(path, body)).toEqual([]);
    }
  });

  it("accepts what a newer build adds or moves", () => {
    const after = structuredClone(rules);
    (at(after.rules, 0) as Record<string, unknown>).group_default = null;
    expect(compareDump("/api/rules", rules, after)).toEqual([]);
    const moved = structuredClone(workspaces);
    at(moved.workspaces, 0).status = "stopped";
    expect(compareDump("/api/workspaces", workspaces, moved)).toEqual([]);
    const retried = structuredClone(pending);
    at(retried.requests, 0).attempts = 4;
    at(retried.requests, 0).last_seen = 99;
    expect(compareDump("/api/pending", pending, retried)).toEqual([]);
    const recomputed = structuredClone(settings);
    recomputed.revision = "b";
    recomputed.effective = { memory: 2048 };
    expect(compareDump("/api/settings", settings, recomputed)).toEqual([]);
    const reread = structuredClone(identities);
    at(at(reread.identities, 0).credentials, 0).last_check = { checked_at: 2 };
    expect(compareDump("/api/identities", identities, reread)).toEqual([]);
  });

  it("does not care in which order a list comes back", () => {
    const after = { rules: [...rules.rules].reverse() };
    expect(compareDump("/api/rules", rules, after)).toEqual([]);
  });
});

describe("an upgrade that loses something", () => {
  it("names a rule that is gone", () => {
    const after = { rules: rules.rules.slice(0, 1) };
    expect(compareDump("/api/rules", rules, after)).toEqual([
      "GET /api/rules .count: was 2, is 1",
      "GET /api/rules .rules: 2 entries became 1",
    ]);
  });

  it("names a rule whose effect or expiry changed", () => {
    const after = structuredClone(rules);
    at(after.rules, 1).effect = "allow";
    (at(after.rules, 0) as Record<string, unknown>).expires_at = 5;
    const losses = compareDump("/api/rules", rules, after);
    expect(losses).toContain(
      "GET /api/rules .rules[0].expires_at: was null, is 5",
    );
    expect(losses).toContain(
      'GET /api/rules .rules[1].effect: was "deny", is "allow"',
    );
  });

  it("names a workspace that is gone or renamed", () => {
    expect(
      compareDump("/api/workspaces", workspaces, { workspaces: [] }),
    ).not.toEqual([]);
    const renamed = { workspaces: [{ id: "w1", name: "other" }] };
    expect(compareDump("/api/workspaces", workspaces, renamed)).toEqual([
      'GET /api/workspaces .workspaces[0].name: was "upgrade-test", is "other"',
    ]);
  });

  it("names a pending request that vanished or was decided", () => {
    expect(compareDump("/api/pending", pending, { requests: [] })).not.toEqual(
      [],
    );
    const decided = structuredClone(pending);
    at(decided.requests, 0).state = "approved";
    expect(compareDump("/api/pending", pending, decided)).toEqual([
      'GET /api/pending .requests[0].state: was "requested", is "approved"',
    ]);
  });

  it("names an environment variable or secret that is gone or lost its hosts", () => {
    const gone = { variables: env.variables.slice(0, 1) };
    expect(compareDump("/api/env", env, gone)).not.toEqual([]);
    const hosts = structuredClone(env);
    at(hosts.variables, 1).hosts = [];
    expect(compareDump("/api/env", env, hosts)).toEqual([
      "GET /api/env .variables[1].hosts: 1 entries became 0",
    ]);
  });

  it("names a setting that went back to its default", () => {
    const after = structuredClone(settings);
    after.ui.theme = "system";
    after.workspace_defaults.memory = null as unknown as number;
    const losses = compareDump("/api/settings", settings, after);
    expect(losses).toContain(
      'GET /api/settings .ui.theme: was "dark", is "system"',
    );
    expect(losses).toContain(
      "GET /api/settings .workspace_defaults.memory: was 3072, is null",
    );
  });

  it("names an identity that lost its author or credentials", () => {
    const after = structuredClone(identities);
    at(after.identities, 0).author.email = "";
    at(after.identities, 0).credentials = [];
    const losses = compareDump("/api/identities", identities, after);
    expect(losses.length).toBe(2);
  });

  it("names a dump that was not taken after the upgrade", () => {
    expect(compareDumps({ "/api/rules": rules }, {})).toEqual([
      "GET /api/rules: not dumped after the upgrade",
    ]);
  });
});

describe("the build after the upgrade", () => {
  it("compares versions as numbers", () => {
    expect(compareVersions("0.9.1", "0.10.0")).toBe(-1);
    expect(compareVersions("0.0.0", "0.0.0")).toBe(0);
    expect(compareVersions("1.0.0-pre", "0.9.9")).toBe(1);
    expect(compareVersions("junk", "0.0.0")).toBeNaN();
  });

  it("accepts the same or a newer build", () => {
    const old = { version: "0.0.0", api_version: "0.31.0" };
    expect(buildIsNotOlder(old, old)).toEqual([]);
    expect(
      buildIsNotOlder(old, { version: "0.1.0", api_version: "0.32.0" }),
    ).toEqual([]);
  });

  it("names a step back and a version that is no version", () => {
    const old = { version: "0.2.0", api_version: "0.31.0" };
    expect(
      buildIsNotOlder(old, { version: "0.1.0", api_version: "0.30.0" }),
    ).toEqual([
      "health.version went from 0.2.0 to 0.1.0",
      "health.api_version went from 0.31.0 to 0.30.0",
    ]);
    expect(
      buildIsNotOlder(old, { version: "", api_version: "0.31.0" }),
    ).toEqual(["health.version went from 0.2.0 to "]);
  });
});
