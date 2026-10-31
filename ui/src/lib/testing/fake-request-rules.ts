// SPDX-License-Identifier: GPL-3.0-or-later
// A stand-in for the API's request-rules endpoints, for unit and component tests, in the shape
// openapi-fetch returns them. Not shipped (tests import it).
import type { RequestRule } from "#lib/rules/request-rules.ts";

export function requestRule(
  id: number,
  over: Partial<RequestRule> = {},
): RequestRule {
  return {
    id,
    host: `h${id}.example.com`,
    methods: [],
    path: "/**",
    workspace: null,
    created_at: 1_000_000 + id,
    created_by: "api",
    warning: null,
    ...over,
  };
}

export class FakeRequestRules {
  rules: RequestRule[] = [];
  calls: string[] = [];
  bodies: unknown[] = [];
  down = false;
  /** Refuse the next write with this status and message. */
  refuse: { status: number; message: string } | null = null;
  #id = 500;

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

  GET = async (path: string) => {
    this.calls.push(`GET ${path}`);
    if (this.down) throw new TypeError("down");
    return this.reply(200, { rules: this.rules });
  };

  POST = async (path: string, init: { body: Record<string, unknown> }) => {
    this.calls.push(`POST ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    if (this.refuse) return this.reply(this.refuse.status);
    const created = requestRule(this.#id++, {
      host: String(init.body["host"]),
      methods: ((init.body["methods"] as string[] | undefined) ?? []).map((m) =>
        m.toUpperCase(),
      ),
      path: String(init.body["path"]),
      workspace: (init.body["workspace"] as string | null) ?? null,
    });
    this.rules.push(created);
    return this.reply(201, created);
  };

  DELETE = async (path: string, init: { params: { path: { id: number } } }) => {
    this.calls.push(`DELETE ${path} ${init.params.path.id}`);
    if (this.down) throw new TypeError("down");
    if (this.refuse) return this.reply(this.refuse.status);
    const found = this.rules.find((r) => r.id === init.params.path.id);
    if (!found) return this.reply(404);
    this.rules = this.rules.filter((r) => r !== found);
    return this.reply(200, found);
  };
}
