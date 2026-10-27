// SPDX-License-Identifier: GPL-3.0-or-later
// A stand-in for the environment, shell init and ports endpoints (and the rule list the delete
// dialog counts), for unit and component tests, in the shape openapi-fetch returns them. It keeps
// real state, so a change shows in the next read, and it never returns a secret's value: those are
// kept in `secretValues`, where a test can look. Not shipped (tests import it).
import type { components } from "#lib/api/schema.d.ts";
import type { EnvVariable } from "#lib/environment/model.ts";
import type { Rule } from "#lib/rules/model.ts";
import type { PortEntry } from "#lib/workspaces/ports.ts";

type Init = {
  params?: { path: Record<string, string | number> };
  body?: Record<string, unknown>;
};

export function variable(
  name: string,
  over: Partial<EnvVariable> = {},
): EnvVariable {
  return {
    name,
    kind: "plain",
    scope: "workspace",
    value: "1",
    hosts: [],
    overridden: false,
    changed_at: 1_000_000,
    ...over,
  };
}

export function secret(
  name: string,
  hosts: string[],
  over: Partial<EnvVariable> = {},
): EnvVariable {
  return variable(name, { kind: "secret", value: null, hosts, ...over });
}

export function port(guest: number, over: Partial<PortEntry> = {}): PortEntry {
  return {
    guest_port: guest,
    listening: true,
    forwarded: false,
    pinned: false,
    host_port: null,
    problem: null,
    ...over,
  };
}

export class FakeEnvApi {
  global: EnvVariable[] = [];
  own: Record<string, EnvVariable[]> = {};
  secretValues: Record<string, string> = {};
  shell: Record<string, { content: string; changed_at: number | null }> = {};
  /** What a save says about its check. */
  shellChecked: { checked: boolean; reason: string | null } = {
    checked: true,
    reason: null,
  };
  ports: Record<string, { running: boolean; ports: PortEntry[] }> = {};
  rules: Rule[] = [];
  calls: string[] = [];
  bodies: unknown[] = [];
  down = false;
  /** Refuse the next write with this status and message. */
  refuse: { status: number; message: string } | null = null;
  #clock = 2_000_000;

  private reply(status: number, data?: unknown, message = "refused") {
    const response = { status, ok: status < 400 } as Response;
    if (status < 400) return { data, response };
    return { error: { error: "invalid", message }, response };
  }

  private refusal() {
    const r = this.refuse;
    this.refuse = null;
    return r ? this.reply(r.status, undefined, r.message) : null;
  }

  #name(init: Init): string {
    return String(init.params?.path["id"]);
  }

  #view(name: string): EnvVariable[] {
    const own = this.own[name] ?? [];
    const names = new Set(own.map((v) => v.name));
    return [
      ...own,
      ...this.global.map((g) => ({ ...g, overridden: names.has(g.name) })),
    ];
  }

  #set(list: EnvVariable[], name: string, body: Record<string, unknown>) {
    const old = list.find((v) => v.name === name);
    let entry: EnvVariable;
    if (body["kind"] === "plain") {
      entry = variable(name, { value: String(body["value"]) });
    } else {
      const given = body["value"];
      if (typeof given === "string" && given !== "") {
        this.secretValues[name] = given;
      } else if (old?.kind !== "secret") {
        return this.reply(422, undefined, "a new secret needs its value");
      }
      entry = secret(name, body["hosts"] as string[]);
    }
    entry = { ...entry, changed_at: this.#clock++ };
    const at = list.findIndex((v) => v.name === name);
    if (at >= 0) list[at] = entry;
    else list.push(entry);
    return this.reply(200, entry);
  }

  GET = async (path: string, init: Init = {}) => {
    this.calls.push(`GET ${path}`);
    if (this.down) throw new TypeError("down");
    switch (path) {
      case "/api/env":
        return this.reply(200, { variables: this.global });
      case "/api/workspaces/{id}/env": {
        const name = this.#name(init);
        return this.reply(200, {
          workspace: name,
          variables: this.#view(name),
        });
      }
      case "/api/workspaces/{id}/shell-init": {
        const name = this.#name(init);
        const saved = this.shell[name];
        return this.reply(200, {
          workspace: name,
          saved: saved !== undefined,
          content: saved?.content ?? "",
          changed_at: saved?.changed_at ?? null,
        });
      }
      case "/api/workspaces/{id}/ports": {
        const state = this.ports[this.#name(init)] ?? {
          running: false,
          ports: [],
        };
        // A copy, as the wire gives one: the page compares by identity.
        return this.reply(200, structuredClone(state));
      }
      case "/api/rules":
        return this.reply(200, { rules: this.rules });
    }
    return this.reply(404);
  };

  PUT = async (path: string, init: Init = {}) => {
    this.calls.push(`PUT ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal();
    if (refused) return refused;
    const body = init.body ?? {};
    const name = this.#name(init);
    switch (path) {
      case "/api/env/{name}":
        return this.#set(this.global, String(init.params?.path["name"]), body);
      case "/api/workspaces/{id}/env/{name}":
        return this.#set(
          (this.own[name] ??= []),
          String(init.params?.path["name"]),
          body,
        );
      case "/api/workspaces/{id}/shell-init": {
        const content = String(body["content"]);
        this.shell[name] = { content, changed_at: this.#clock++ };
        const saved: components["schemas"]["ShellInitSaved"] = {
          checked: this.shellChecked.checked,
          unchecked_reason: this.shellChecked.reason,
          shell_init: {
            workspace: name,
            saved: true,
            content,
            changed_at: this.shell[name].changed_at,
          },
        };
        return this.reply(200, saved);
      }
      case "/api/workspaces/{id}/ports/{port}": {
        const state = (this.ports[name] ??= { running: true, ports: [] });
        const guest = Number(init.params?.path["port"]);
        const host = (body["host_port"] as number | null) ?? guest;
        let row = state.ports.find((p) => p.guest_port === guest);
        if (!row) {
          row = port(guest, { listening: false });
          state.ports.push(row);
        }
        row.forwarded = true;
        row.pinned = body["pinned"] === true;
        row.host_port = host;
        return this.reply(200, row);
      }
    }
    return this.reply(404);
  };

  DELETE = async (path: string, init: Init = {}) => {
    this.calls.push(`DELETE ${path}`);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal();
    if (refused) return refused;
    const name = this.#name(init);
    const key = String(init.params?.path["name"]);
    const gone = { status: 204, ok: true } as Response;
    switch (path) {
      case "/api/env/{name}":
        this.global = this.global.filter((v) => v.name !== key);
        return { response: gone };
      case "/api/workspaces/{id}/env/{name}":
        this.own[name] = (this.own[name] ?? []).filter((v) => v.name !== key);
        return { response: gone };
      case "/api/workspaces/{id}/shell-init":
        if (!this.shell[name]) return this.reply(404);
        delete this.shell[name];
        return { response: gone };
      case "/api/workspaces/{id}/ports/{port}": {
        const state = this.ports[name];
        const guest = Number(init.params?.path["port"]);
        const row = state?.ports.find((p) => p.guest_port === guest);
        if (!state || !row) return this.reply(404);
        row.forwarded = false;
        row.pinned = false;
        row.host_port = null;
        if (!row.listening) {
          state.ports = state.ports.filter((p) => p.guest_port !== guest);
        }
        return { response: gone };
      }
    }
    return this.reply(404);
  };
}
