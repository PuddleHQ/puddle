// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { connection } from "#lib/testing/fake-audit.ts";
import {
  exportName,
  jsonLine,
  rawJson,
  sentence,
  ruleLink,
  type AuditRecord,
} from "./model.ts";

describe("raw records and export lines", () => {
  const { record } = connection(1, { host: 'a"b.test' });
  it("indents the raw record and keeps a hostile host as text", () => {
    expect(rawJson(record)).toContain('\n  "host": "a\\"b.test"');
    expect(JSON.parse(rawJson(record))).toEqual(record);
  });
  it("writes one line per record, exactly as the API sent it", () => {
    const line = jsonLine(record);
    expect(line.endsWith("\n")).toBe(true);
    expect(line.slice(0, -1)).not.toContain("\n");
    expect(JSON.parse(line)).toEqual(record);
  });
  it("names the file after the moment, with no characters a file system refuses", () => {
    expect(exportName(Date.UTC(2026, 9, 7, 12, 0, 5))).toBe(
      "puddle-activity-2026-10-07T12-00-05.jsonl",
    );
  });
});

describe("ruleLink", () => {
  const deny = (over: Record<string, unknown> = {}) =>
    connection(1, { decision: "deny", host: "ads.example.net", ...over })
      .record;

  it("links a refused connection to the form for a rule, with the workspace when known", () => {
    expect(ruleLink(deny(), "shop")).toBe(
      "/rules?new-rule=ads.example.net&workspace=shop",
    );
    expect(ruleLink(deny(), null)).toBe("/rules?new-rule=ads.example.net");
  });

  it("offers nothing where no rule could help", () => {
    expect(ruleLink(deny({ decision: "allow" }), "shop")).toBeNull();
    expect(ruleLink(deny({ decision: "blocked" }), "shop")).toBeNull();
    expect(ruleLink(deny({ host: null }), "shop")).toBeNull();
    expect(
      ruleLink(
        { type: "quota_changed", ts: 1 } as unknown as AuditRecord,
        null,
      ),
    ).toBeNull();
  });
});

describe("sentence", () => {
  const view = (destination: string | null, detail: string | null) =>
    ({
      destination,
      detail,
      outcome: { label: "Denied" },
      type: "connection",
    }) as unknown as Parameters<typeof sentence>[0];

  it("joins what the row says after its head, skipping what is empty", () => {
    expect(sentence(view("a.test:443", "no rule"))).toBe(
      "Denied: a.test:443, no rule",
    );
    expect(sentence(view("a.test:443", ""))).toBe("Denied: a.test:443");
    expect(sentence(view(null, "no rule"))).toBe("Denied: no rule");
    expect(sentence(view(null, null))).toBe("Denied");
  });

  it("falls back to the record type when there is no outcome", () => {
    const bare = { ...view(null, null), outcome: null } as never;
    expect(sentence(bare)).toBe("connection");
  });
});
