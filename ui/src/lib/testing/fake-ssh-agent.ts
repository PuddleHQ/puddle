// SPDX-License-Identifier: AGPL-3.0-or-later
// A stand-in for the API's SSH agent endpoints (the first-use questions and the per-host switch),
// answering in the shape openapi-fetch returns. Not shipped (tests import it).
import type { components } from "#lib/api/schema.d.ts";

type Ask = components["schemas"]["SshAgentAskView"];
type Host = components["schemas"]["SshAgentHostView"];

export const KEY_A = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

export function ask(id: number, over: Partial<Ask> = {}): Ask {
  return {
    id,
    workspace: "demo" as Ask["workspace"],
    host: `build${id}.example.com`,
    key_type: "ssh-ed25519",
    fingerprint: KEY_A,
    first_seen: 1_000_000 + id,
    last_seen: 1_000_000 + id,
    attempts: 1,
    ...over,
  };
}

type Init = {
  params?: { path?: Record<string, unknown> };
  body?: { answer?: string; keys?: string[] };
};

export class FakeSshAgent {
  asks: Ask[] = [];
  hosts: Record<string, Host[]> = {};
  calls: string[] = [];
  answers: { id: number; answer: string }[] = [];
  /** Set to make every call fail the way a stopped service does. */
  down = false;
  /** Status for the next call that changes something, then back to success. */
  nextStatus: number | null = null;

  private reply(status: number, data?: unknown) {
    const response = { status, ok: status < 400 } as Response;
    return status < 400
      ? { data, response }
      : { error: { error: "refused", message: "refused" }, response };
  }

  GET = async (path: string, init?: Init) => {
    this.calls.push(`GET ${path}`);
    if (this.down) throw new TypeError("down");
    if (path === "/api/ssh-agent-asks") {
      return this.reply(200, { asks: this.asks });
    }
    if (path === "/api/workspaces/{id}/ssh-agent") {
      const id = String(init?.params?.path?.["id"]);
      return this.reply(200, { workspace: id, hosts: this.hosts[id] ?? [] });
    }
    throw new Error(`unexpected GET ${path}`);
  };

  POST = async (path: string, init: Init) => {
    this.calls.push(`POST ${path}`);
    if (this.down) throw new TypeError("down");
    const status = this.nextStatus;
    this.nextStatus = null;
    if (status !== null) return this.reply(status);
    const id = Number(init.params?.path?.["ask"]);
    if (!this.asks.some((a) => a.id === id)) return this.reply(404);
    this.answers.push({ id, answer: String(init.body?.answer) });
    this.asks = this.asks.filter((a) => a.id !== id);
    return this.reply(204);
  };

  PUT = async (path: string, init: Init) => {
    this.calls.push(`PUT ${path}`);
    if (this.down) throw new TypeError("down");
    const status = this.nextStatus;
    this.nextStatus = null;
    if (status !== null) return this.reply(status);
    const id = String(init.params?.path?.["id"]);
    const host = String(init.params?.path?.["host"]);
    const row: Host = {
      host: host.toLowerCase(),
      keys: init.body?.keys ?? null,
      changed_at: 1,
    };
    this.hosts[id] = [
      ...(this.hosts[id] ?? []).filter((h) => h.host !== row.host),
      row,
    ].sort((a, b) => a.host.localeCompare(b.host));
    return this.reply(200, row);
  };

  DELETE = async (path: string, init: Init) => {
    this.calls.push(`DELETE ${path}`);
    if (this.down) throw new TypeError("down");
    const status = this.nextStatus;
    this.nextStatus = null;
    if (status !== null) return this.reply(status);
    const id = String(init.params?.path?.["id"]);
    const host = String(init.params?.path?.["host"]);
    const known = (this.hosts[id] ?? []).some((h) => h.host === host);
    this.hosts[id] = (this.hosts[id] ?? []).filter((h) => h.host !== host);
    return known ? this.reply(204) : this.reply(404);
  };
}
