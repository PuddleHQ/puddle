// SPDX-License-Identifier: AGPL-3.0-or-later
// A stand-in for the API's rule set endpoints, for unit and component tests, in the shape
// openapi-fetch returns them. Not shipped (tests import it).
import type { Fill } from "#lib/rules/fill.ts";
import type { RuleSet, SystemHost } from "#lib/rules/sets.ts";

export function builtIn(slug: string, over: Partial<RuleSet> = {}): RuleSet {
  return {
    id: `builtin:${slug}`,
    kind: "built_in",
    name: slug,
    description: `The ${slug} hosts.`,
    fill: { kind: "solid", color: "#2f80c9" },
    default_on: false,
    global: null,
    overrides: [],
    entries: [
      {
        pattern: `${slug}.example`,
        pattern_kind: "exact",
        effect: "allow",
        note: "main host",
        enabled: true,
        rule_id: null,
        expires_at: null,
        precedence: 0,
      },
    ],
    entry_count: 1,
    changed_at: null,
    created_at: null,
    default_precedence: null,
    ...over,
  };
}

/** `n` deny entries `h0.tracker.example` and on, for a set too big to list whole. */
export function trackerEntries(n: number): RuleSet["entries"] {
  return Array.from({ length: n }, (_, i) => ({
    pattern: `h${i}.tracker.example`,
    pattern_kind: "exact" as const,
    effect: "deny" as const,
    note: "",
    enabled: true,
    rule_id: null,
    expires_at: null,
    precedence: 0,
  }));
}

export function mine(id: number, over: Partial<RuleSet> = {}): RuleSet {
  return {
    id: `user:${id}`,
    kind: "user",
    name: `Set ${id}`,
    description: "",
    fill: { kind: "solid", color: "#2ea06e" },
    default_on: true,
    global: null,
    overrides: [],
    entries: [],
    entry_count: 0,
    changed_at: null,
    created_at: 1_000,
    default_precedence: null,
    ...over,
  };
}

export function systemHost(
  pattern: string,
  over: Partial<SystemHost> = {},
): SystemHost {
  return {
    pattern,
    note: "extensions",
    reason: "code_server",
    reason_text: "The browser editor runs the bundled code-server.",
    workspace: null,
    ...over,
  };
}

type Body = Record<string, unknown>;
type Init = { params?: { path: { id: string } }; body?: Body };

export class FakeRuleSets {
  sets: RuleSet[] = [];
  system: SystemHost[] = [];
  calls: string[] = [];
  bodies: unknown[] = [];
  down = false;
  /** Waiting requests the next switch closes. */
  closes: number[] = [];
  /** Refuse the next write with this status and message. */
  refuse: { status: number; message: string } | null = null;
  #id = 50;

  private reply(status: number, data?: unknown) {
    const response = { status, ok: status < 400 } as Response;
    if (status < 400) return { data, response };
    const refusal = this.refuse;
    this.refuse = null;
    return {
      error: {
        error: status === 422 ? "invalid" : "internal",
        message: refusal?.message ?? "refused",
      },
      response,
    };
  }

  #find(init: Init): RuleSet | undefined {
    return this.sets.find((s) => s.id === init.params?.path.id);
  }

  /** Every entry of a big built-in set, by set id, for the entries endpoint. */
  full: Record<string, RuleSet["entries"]> = {};

  GET = async (path: string, init?: Init) => {
    this.calls.push(`GET ${path}`);
    if (this.down) throw new TypeError("down");
    if (path.endsWith("/entries")) {
      const query = init?.params as
        { query?: { q?: string; offset?: number } } | undefined;
      const q = (query?.query?.q ?? "").toLowerCase();
      const offset = query?.query?.offset ?? 0;
      const all = (this.full[init?.params?.path.id ?? ""] ?? []).filter((e) =>
        e.pattern.toLowerCase().includes(q),
      );
      return this.reply(200, {
        entries: all.slice(offset, offset + 200),
        total: all.length,
      });
    }
    return this.reply(200, { sets: this.sets, system_managed: this.system });
  };

  POST = async (path: string, init: Init) => {
    this.calls.push(`POST ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    if (this.refuse) return this.reply(this.refuse.status);
    const made = mine(this.#id++, {
      name: String(init.body?.["name"]),
      description: String(init.body?.["description"] ?? ""),
      ...(init.body?.["fill"] ? { fill: init.body["fill"] as Fill } : {}),
    });
    this.sets = [...this.sets, made];
    return this.reply(201, made);
  };

  PUT = async (path: string, init: Init) => {
    this.calls.push(`PUT ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    if (this.refuse) return this.reply(this.refuse.status);
    const found = this.#find(init);
    if (!found) return this.reply(404);
    let next: RuleSet;
    if (path.endsWith("/switch")) {
      const workspace = (init.body?.["workspace"] as string | null) ?? null;
      const enabled = init.body?.["enabled"] as boolean | null;
      if (workspace === null) next = { ...found, global: enabled };
      else {
        const rest = found.overrides.filter((o) => o.workspace !== workspace);
        next = {
          ...found,
          overrides:
            enabled === null
              ? rest
              : [...rest, { workspace: workspace as never, enabled }],
        };
      }
      this.sets = this.sets.map((s) => (s.id === found.id ? next : s));
      const closed = this.closes;
      this.closes = [];
      return this.reply(200, { set: next, closed });
    }
    next = {
      ...found,
      name: String(init.body?.["name"]),
      description: String(init.body?.["description"] ?? ""),
      ...(init.body?.["fill"] ? { fill: init.body["fill"] as Fill } : {}),
    };
    this.sets = this.sets.map((s) => (s.id === found.id ? next : s));
    return this.reply(200, next);
  };

  DELETE = async (path: string, init: Init) => {
    this.calls.push(`DELETE ${path}`);
    if (this.down) throw new TypeError("down");
    if (this.refuse) return this.reply(this.refuse.status);
    const found = this.#find(init);
    this.sets = this.sets.filter((s) => s !== found);
    return found ? this.reply(200, found) : this.reply(404);
  };
}
