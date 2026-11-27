// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { connection } from "#lib/testing/fake-audit.ts";
import { describe as row, type AuditRecord } from "./model.ts";

const rule = {
  id: 7,
  pattern: ".example.com",
  pattern_kind: "suffix",
  scope: "workspace",
  workspace_id: "shop",
  effect: "allow",
  expires_at: null,
  created_at: 1,
  created_by: "ui",
  source_pending_id: null,
  precedence: 5,
};
const pending = {
  id: 3,
  workspace_id: "shop",
  host: "evil.example.org",
  port: 8443,
  first_seen: 1,
  last_seen: 2,
  attempts: 1,
  state: "requested",
  decided_at: null,
  decided_by: null,
  rule_id: null,
};

describe("rule set records (R-43)", () => {
  const set = {
    id: 4,
    name: "Client X",
    description: "",
    created_at: 1,
    created_by: "ui",
  };
  it("shows a set made, renamed or deleted by name", () => {
    for (const type of [
      "rule_set_created",
      "rule_set_updated",
      "rule_set_deleted",
    ]) {
      expect(
        row({
          type,
          ts: 1,
          rule_set: set,
          before: set,
          actor: "api",
        } as AuditRecord),
      ).toMatchObject({
        workspace: null,
        destination: "Client X",
        detail: "user:4, by api",
      });
    }
  });
  it("shows a switch for every workspace or one", () => {
    const switched = (workspace: string | null, enabled: boolean | null) =>
      row({
        type: "rule_set_switched",
        ts: 1,
        set_id: "builtin:github",
        workspace_id: workspace,
        enabled,
        actor: "ui",
      } as AuditRecord);
    expect(switched(null, true)).toMatchObject({
      workspace: null,
      destination: "builtin:github",
      detail: "on for every workspace, by ui",
    });
    expect(switched("api", false).detail).toBe("off for workspace api, by ui");
    expect(switched("api", null).detail).toBe(
      "follows the next level for workspace api, by ui",
    );
  });
  it("shows a rule, a method and path rule or a built-in entry switched", () => {
    const switched = (
      kind: "rule" | "request_rule" | "builtin_entry",
      enabled: boolean,
    ) =>
      row({
        type: "rule_switched",
        ts: 1,
        kind,
        id: kind === "builtin_entry" ? null : 4,
        set_id: kind === "builtin_entry" ? "builtin:github" : null,
        pattern: "api.github.com",
        workspace_id: kind === "rule" ? "shop" : null,
        enabled,
        actor: "ui",
      } as AuditRecord);
    expect(switched("rule", false)).toMatchObject({
      workspace: "shop",
      destination: "api.github.com",
      detail: "off, by ui",
    });
    expect(switched("request_rule", true).detail).toBe(
      "on, rule on method and path, by ui",
    );
    expect(switched("builtin_entry", false).detail).toBe(
      "off in builtin:github, by ui",
    );
  });
  it("shows what an update changed in a built-in set", () => {
    const changed = (added: string[], removed: string[]) =>
      row({
        type: "rule_set_changed",
        ts: 1,
        set_id: "builtin:github",
        added,
        removed,
      } as AuditRecord).detail;
    expect(changed(["a.example"], ["b.example"])).toBe(
      "added a.example; removed b.example",
    );
    expect(changed(["a.example"], [])).toBe("added a.example");
    expect(changed([], ["b.example"])).toBe("removed b.example");
  });
  it("shows why System managed changed, in words", () => {
    const changed = (added: string[], removed: string[]) =>
      row({
        type: "system_managed_changed",
        ts: 1,
        workspace_id: null,
        added,
        removed,
      } as AuditRecord).detail;
    expect(changed(["microsoft_server"], ["code_server"])).toBe(
      "now for Microsoft's VS Code server; no longer for code-server",
    );
    expect(changed(["direct_ssh", "later"], [])).toBe(
      "now for direct SSH, later",
    );
    expect(changed([], ["direct_ssh"])).toBe("no longer for direct SSH");
  });
  it("names the set that decided a connection or a request", () => {
    expect(
      row(connection(1, { rule_id: null, rule_set: "system" }).record).detail,
    ).toBe("System managed");
    expect(
      row(connection(1, { rule_id: 8, rule_set: "user:2" }).record).detail,
    ).toBe("rule set user:2 · rule 8");
    expect(
      row({
        type: "pending_decided",
        ts: 1,
        pending: {
          ...pending,
          state: "allowed",
          decided_by: "system",
          rule_set: "builtin:github",
        },
      } as AuditRecord).detail,
    ).toBe("by system, rule set builtin:github");
    expect(
      row({
        type: "rule_created",
        ts: 1,
        rule: { ...rule, scope: "set", workspace_id: null, set_id: 4 },
      } as AuditRecord).detail,
    ).toBe("allow in rule set 4, by ui");
  });
});
