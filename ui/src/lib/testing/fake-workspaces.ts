// SPDX-License-Identifier: GPL-3.0-or-later
// A stand-in for the API's workspace and per-workspace settings endpoints, for unit and
// component tests, in the shape openapi-fetch returns them. Not shipped (tests import it).
import type { components } from "#lib/api/schema.d.ts";
import type { DeleteCheck, Workspace } from "#lib/workspaces/model.ts";
import type { Checkout, CheckoutListing } from "#lib/workspaces/checkouts.ts";

type Layer = components["schemas"]["SettingsLayer"];

/** The entry id the fake gives the list entry in `directory`: never the directory itself. */
export function entryOf(directory: string): string {
  return `e-${directory}`;
}

export function workspace(
  name: string,
  over: Partial<Workspace> = {},
): Workspace {
  return {
    id: "w-" + name,
    name,
    repositories: [
      {
        entry: entryOf(name),
        url: `https://github.com/acme/${name}.git`,
        directory: name,
      },
    ],
    start_folder: null,
    allowed_hosts: [],
    session_user: "agent",
    image: "mcr.microsoft.com/devcontainers/base:debian",
    memory_mib: 8192,
    status: "stopped",
    busy: null,
    created_at: 1_000_000,
    disk_size_mib: 32_768,
    disk_used_mib: 2048,
    root_disk_size_mib: 4096,
    root_disk_used_mib: 1024,
    disk_warnings: [],
    direct_ssh: false,
    settings_error: null,
    first_connect_notice_due: false,
    identities: [],
    ...over,
  };
}

/** A checkout the guest holds, with the list entry that has its directory (none by default). */
export function checkout(
  directory: string,
  over: Partial<Checkout> = {},
): Checkout {
  return {
    directory,
    branch: "main",
    head: "a1b2c3d",
    dirty: false,
    entry: null,
    manage: { offered: true, reason: null },
    remote_note: null,
    remote_url: `https://github.com/acme/${directory}.git`,
    unreadable: null,
    ...over,
  };
}

/** What a listing of a running workspace says. */
export function listing(
  checkouts: Checkout[],
  over: Partial<CheckoutListing> = {},
): CheckoutListing {
  return {
    checked_at: 1_000_000,
    checkouts,
    missing: [],
    more: 0,
    problem: null,
    running: true,
    ...over,
  };
}

/** What a delete check of the workspace with this id says. */
export function cleanCheck(
  id: string,
  over: Partial<DeleteCheck> = {},
): DeleteCheck {
  return {
    workspace: id,
    clean: true,
    repos: [],
    other: { items: [], more: 0 },
    errors: [],
    removes_sandbox: null,
    volume_missing: false,
    fingerprint: "fp-clean",
    ...over,
  };
}

export function dirtyCheck(name: string): DeleteCheck {
  return cleanCheck(name, {
    clean: false,
    fingerprint: "fp-dirty",
    repos: [
      {
        dir: name,
        clean: false,
        uncommitted: { items: [" M a.md", "?? b.md"], more: 3 },
        unpushed: { items: ["abc123 Fix it"], more: 0 },
        stashes: { items: [], more: 0 },
      },
    ],
    other: { items: ["scratch"], more: 0 },
  });
}

const noLayer = (): Layer => ({
  capture_logins: null,
  warn_foreign_credentials: null,
  clipboard_read: null,
  direct_ssh: null,
  local_toggles: {
    link_local: null,
    loopback: null,
    metadata: null,
    private: null,
    special: null,
    company_network: null,
  },
  memory: null,
  cpus: null,
  reconnection_grace: null,
  wildcards_reach_local: null,
  zoom_hotkeys: null,
});

const resolved = <T>(value: T, source: "workspace" | "global" | "default") => ({
  value,
  source,
});

export class FakeWorkspaces {
  list: Workspace[] = [];
  overrides: Record<string, Layer> = {};
  /** Unknown fields per workspace document. */
  unknown: Record<string, string[]> = {};
  globalMemory = 8192;
  check: DeleteCheck | null = null;
  calls: string[] = [];
  bodies: unknown[] = [];
  down = false;
  /** What each workspace's checkout listing says; a running workspace with none listed by default. */
  checkouts: Record<string, CheckoutListing> = {};
  /** The logins each workspace keeps, by id; none when unset. */
  kept: Record<
    string,
    {
      logins: components["schemas"]["KeptLoginView"][];
      store_readable: boolean;
    }
  > = {};
  /** How each workspace decrypts hosts, by id; the default (inherit, no names) when unset. */
  interception: Record<
    string,
    {
      mode: components["schemas"]["WorkspaceMode"];
      never_decrypt: string[];
    }
  > = {};
  /** The global interception mode the workspaces that inherit follow. */
  globalInterceptionMode: components["schemas"]["InterceptionMode"] =
    "known_only";
  /** The names no workspace decrypts. */
  globalNeverDecrypt: string[] = [];
  /** The hosts each workspace's tools refused Puddle's certificate for, by workspace id. */
  learned: Record<string, components["schemas"]["LearnedHost"][]> = {};
  /** The background problems the host lists. */
  problems: {
    key: string;
    title: string;
    detail: string;
    workspace: string | null;
  }[] = [];
  /** Refuse the next call to a path (`"POST /api/workspaces"`) with this status and text. */
  refuse = new Map<
    string,
    { status: number; message: string; error?: string }
  >();
  attachReply: components["schemas"]["AttachResponse"] = {
    opened: true,
    message: null,
    open_uri: null,
  };

  private reply(
    status: number,
    data?: unknown,
    message = "refused",
    code?: string,
  ) {
    const response = { status, ok: status < 400 } as Response;
    return status < 400
      ? { data, response }
      : {
          error: {
            error:
              code ??
              (status === 409
                ? "conflict"
                : status === 404
                  ? "not_found"
                  : "invalid"),
            message,
          },
          response,
        };
  }

  private refusal(key: string) {
    const r = this.refuse.get(key);
    if (!r) return null;
    this.refuse.delete(key);
    return this.reply(r.status, undefined, r.message, r.error);
  }

  private find(id: unknown) {
    return this.list.find((w) => w.id === id);
  }

  private effective(name: string) {
    const o = this.overrides[name] ?? noLayer();
    const pick = (v: boolean | null) =>
      v === null ? resolved(false, "default") : resolved(v, "workspace");
    return {
      clipboard_read:
        o.clipboard_read === null
          ? resolved("ask" as const, "default")
          : resolved(o.clipboard_read, "workspace"),
      local_toggles: {
        link_local: pick(o.local_toggles.link_local),
        loopback: pick(o.local_toggles.loopback),
        metadata: pick(o.local_toggles.metadata),
        private: pick(o.local_toggles.private),
        special: pick(o.local_toggles.special),
        company_network: pick(o.local_toggles.company_network),
      },
      memory:
        o.memory === null
          ? resolved(this.globalMemory, "global")
          : resolved(o.memory, "workspace"),
      cpus: resolved(o.cpus ?? 1, o.cpus === null ? "default" : "workspace"),
      reconnection_grace: resolved(300, "default"),
      wildcards_reach_local: resolved(false, "default"),
      zoom_hotkeys: resolved(true, "default"),
      direct_ssh: pick(o.direct_ssh),
      capture_logins:
        o.capture_logins === null
          ? resolved(true, "default")
          : resolved(o.capture_logins, "workspace"),
      warn_foreign_credentials:
        o.warn_foreign_credentials === null
          ? resolved(true, "default")
          : resolved(o.warn_foreign_credentials, "workspace"),
    };
  }

  GET = async (
    path: string,
    init?: { params?: { path?: Record<string, unknown> } },
  ) => {
    this.calls.push(`GET ${path}`);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`GET ${path}`);
    if (refused) return refused;
    const id = init?.params?.path?.["id"] ?? init?.params?.path?.["workspace"];
    switch (path) {
      case "/api/workspaces":
        return this.reply(200, { workspaces: this.list });
      case "/api/workspaces/{id}/delete-check":
        return this.find(id)
          ? this.reply(200, this.check ?? cleanCheck(String(id)))
          : this.reply(404, undefined, "no such workspace");
      case "/api/workspaces/{id}/checkouts": {
        const found = this.find(id);
        if (!found) return this.reply(404, undefined, "no such workspace");
        return this.reply(
          200,
          this.checkouts[String(id)] ??
            listing([], {
              running: found.status === "running",
              checked_at: found.status === "running" ? 1_000_000 : null,
              problem:
                found.status === "running"
                  ? null
                  : "the workspace is not running; start it to list its checkouts",
            }),
        );
      }
      case "/api/workspaces/{id}/logins":
        return this.find(id)
          ? this.reply(
              200,
              this.kept[String(id)] ?? { logins: [], store_readable: true },
            )
          : this.reply(404, undefined, "no such workspace");
      case "/api/workspaces/{id}/interception": {
        const found = this.find(id);
        return found
          ? this.reply(200, this.interceptionView(found))
          : this.reply(404, undefined, "no such workspace");
      }
      case "/api/workspaces/{id}/interception/learned": {
        const found = this.find(id);
        return found
          ? this.reply(200, { hosts: this.learned[found.id] ?? [] })
          : this.reply(404, undefined, "no such workspace");
      }
      case "/api/interception/never-decrypt":
        return this.reply(200, { never_decrypt: this.globalNeverDecrypt });
      case "/api/problems":
        return this.reply(200, { problems: this.problems });
      case "/api/settings":
        return this.reply(200, {
          effective: this.effective("-"),
          workspace_defaults: noLayer(),
          unknown_fields: [],
          vscode_server: {},
        });
      case "/api/settings/workspaces/{workspace}": {
        const key = String(id);
        return this.reply(200, {
          workspace: key,
          overrides: this.overrides[key] ?? noLayer(),
          effective: this.effective(key),
          unknown_fields: this.unknown[key] ?? [],
        });
      }
    }
    throw new Error(`unexpected GET ${path}`);
  };

  POST = async (
    path: string,
    init: {
      params?: { path?: Record<string, unknown> };
      body?: Record<string, unknown>;
    },
  ) => {
    this.calls.push(`POST ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`POST ${path}`);
    if (refused) return refused;
    if (path === "/api/workspaces") {
      const body = init.body ?? {};
      const name = String(body["name"]);
      if (this.list.some((w) => w.name.toLowerCase() === name.toLowerCase()))
        return this.reply(
          409,
          undefined,
          `a workspace named ${name} already exists`,
        );
      const asked = (body["repositories"] ?? []) as { url: string }[];
      const created = workspace(name, {
        repositories: asked.map((r) => ({
          entry: entryOf(
            r.url
              .replace(/\/+$/, "")
              .split("/")
              .pop()!
              .replace(/\.git$/, ""),
          ),
          url: r.url,
          directory: r.url
            .replace(/\/+$/, "")
            .split("/")
            .pop()!
            .replace(/\.git$/, ""),
        })),
        status: "created",
        busy: "creating",
        memory_mib: Number(body["memory_mib"] ?? 8192),
      });
      this.list.push(created);
      return this.reply(202, created);
    }
    const id = init.params?.path?.["id"];
    const found = this.find(id);
    if (!found) return this.reply(404, undefined, "no such workspace");
    if (path === "/api/workspaces/{id}/interception/learned/{host}/permanent") {
      const host = String(init.params?.path?.["host"]);
      const had = this.learned[found.id] ?? [];
      if (!had.some((l) => l.host === host))
        return this.reply(404, undefined, "not on the learned list");
      if (init.body?.["scope"] === "global") {
        this.globalNeverDecrypt = [...this.globalNeverDecrypt, host].sort();
      } else {
        const own = this.interception[found.id] ?? {
          mode: "inherit" as const,
          never_decrypt: [],
        };
        this.interception[found.id] = {
          ...own,
          never_decrypt: [...own.never_decrypt, host].sort(),
        };
      }
      this.learned[found.id] = had.filter((l) => l.host !== host);
      return { data: undefined, response: { status: 204, ok: true } };
    }
    if (path === "/api/workspaces/{id}/repositories") {
      const body = init.body ?? {};
      const url = String(body["url"]);
      const directory = String(
        body["directory"] ??
          url
            .replace(/\/+$/, "")
            .split("/")
            .pop()!
            .replace(/\.git$/, ""),
      );
      const cloning = body["kind"] === "clone";
      const next: Workspace = {
        ...found,
        repositories: [
          ...found.repositories,
          { entry: entryOf(directory), url, directory },
        ],
        ...(cloning ? { busy: "cloning" as const } : {}),
      };
      this.list = this.list.map((w) => (w.id === found.id ? next : w));
      return this.reply(cloning ? 202 : 200, next);
    }
    if (path === "/api/workspaces/{id}/repositories/{entry}/clone") {
      const next: Workspace = { ...found, busy: "cloning" };
      this.list = this.list.map((w) => (w.id === found.id ? next : w));
      return this.reply(202, next);
    }
    if (path === "/api/workspaces/{id}/grow-root-disk") {
      const size = Number(init.body?.["size_mib"]);
      const current = found.root_disk_size_mib ?? 0;
      if (size <= current)
        return this.reply(
          400,
          undefined,
          `a root disk only grows: ${size} MiB is not above its ${current} MiB`,
        );
      const next: Workspace = {
        ...found,
        busy: "growing_root_disk",
        root_disk_size_mib: size,
        disk_warnings: found.disk_warnings.filter((k) => k !== "root"),
      };
      this.list = this.list.map((w) => (w.id === found.id ? next : w));
      return this.reply(202, next);
    }
    const verb = path.split("/").pop();
    if (verb === "attach") return this.reply(200, this.attachReply);
    const next: Workspace =
      verb === "start"
        ? { ...found, status: "starting", busy: "starting" }
        : verb === "stop"
          ? { ...found, status: "draining", busy: "stopping" }
          : verb === "reset-root-disk"
            ? { ...found, busy: "resetting_root_disk" }
            : { ...found, busy: "reclaiming" };
    this.list = this.list.map((w) => (w.id === found.id ? next : w));
    return this.reply(202, next);
  };

  /** `PATCH /api/workspaces/{id}`: the rename, with the API's clash rule (409, ignoring case). */
  PATCH = async (
    path: string,
    init: {
      params: { path: { id: string } };
      body: { name: string };
    },
  ) => {
    this.calls.push(`PATCH ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`PATCH ${path}`);
    if (refused) return refused;
    const found = this.find(init.params.path.id);
    if (!found) return this.reply(404, undefined, "no such workspace");
    const shown = init.body.name.trim();
    const clash = this.list.find(
      (w) => w.id !== found.id && w.name.toLowerCase() === shown.toLowerCase(),
    );
    if (clash)
      return this.reply(
        409,
        undefined,
        `another workspace is already called ${clash.name}; choose a different name`,
      );
    const next: Workspace = { ...found, name: shown };
    this.list = this.list.map((w) => (w.id === found.id ? next : w));
    return this.reply(200, next);
  };

  DELETE = async (
    path: string,
    init: { params: { path: { id: string } }; body: Record<string, unknown> },
  ) => {
    this.calls.push(`DELETE ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`DELETE ${path}`);
    if (refused) return refused;
    const found = this.find(init.params.path.id);
    if (!found) return this.reply(404, undefined, "no such workspace");
    if (path === "/api/workspaces/{id}/interception/learned/{host}") {
      const gone = (init.params.path as { host?: string }).host;
      const had = this.learned[found.id] ?? [];
      const live = had.some((l) => l.host === gone && l.passed_through);
      this.learned[found.id] = had.map((l) =>
        l.host === gone
          ? { ...l, passed_through: false, expires_in_secs: null }
          : l,
      );
      return live
        ? { data: undefined, response: { status: 204, ok: true } }
        : this.reply(404, undefined, "not passed through");
    }
    if (path === "/api/workspaces/{id}/interception/learned") {
      this.learned[found.id] = (this.learned[found.id] ?? []).map((l) => ({
        ...l,
        passed_through: false,
        expires_in_secs: null,
      }));
      return { data: undefined, response: { status: 204, ok: true } };
    }
    if (path === "/api/workspaces/{id}/logins/{service}") {
      const gone = (init.params.path as { service?: string }).service;
      const kept = this.kept[found.id];
      if (kept) {
        this.kept[found.id] = {
          ...kept,
          logins: kept.logins.filter((l) => l.service !== gone),
        };
      }
      return { data: undefined, response: { status: 204, ok: true } };
    }
    if (path === "/api/workspaces/{id}/repositories/{entry}") {
      const gone = (init.params.path as { entry?: string }).entry;
      const next: Workspace = {
        ...found,
        repositories: found.repositories.filter((r) => r.entry !== gone),
      };
      this.list = this.list.map((w) => (w.id === found.id ? next : w));
      return this.reply(200, next);
    }
    const next: Workspace = { ...found, busy: "deleting" };
    this.list = this.list.map((w) => (w.id === found.id ? next : w));
    return this.reply(202, next);
  };

  private interceptionView(w: Workspace) {
    const own = this.interception[w.id] ?? {
      mode: "inherit" as const,
      never_decrypt: [],
    };
    return {
      workspace: w.id,
      mode: own.mode,
      effective_mode:
        own.mode === "inherit" ? this.globalInterceptionMode : own.mode,
      never_decrypt: own.never_decrypt,
      global_never_decrypt: this.globalNeverDecrypt,
    };
  }

  PUT = async (
    path: string,
    init: {
      params: { path: { workspace: string } };
      body: { overrides: Layer };
    },
  ) => {
    this.calls.push(`PUT ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`PUT ${path}`);
    if (refused) return refused;
    if (path === "/api/interception/never-decrypt") {
      const asked = init as unknown as { body: { never_decrypt: string[] } };
      this.globalNeverDecrypt = [...asked.body.never_decrypt].sort();
      return this.reply(200, { never_decrypt: this.globalNeverDecrypt });
    }
    if (path === "/api/workspaces/{id}/interception") {
      const asked = init as unknown as {
        params: { path: { id: string } };
        body: {
          mode?: components["schemas"]["WorkspaceMode"];
          never_decrypt?: string[];
        };
      };
      const found = this.find(asked.params.path.id);
      if (!found) return this.reply(404, undefined, "no such workspace");
      const before = this.interceptionView(found);
      this.interception[found.id] = {
        mode: asked.body.mode ?? before.mode,
        never_decrypt: asked.body.never_decrypt
          ? [...asked.body.never_decrypt].sort()
          : before.never_decrypt,
      };
      return this.reply(200, this.interceptionView(found));
    }
    if (path === "/api/workspaces/{id}/start-folder") {
      const asked = init as unknown as {
        params: { path: { id: string } };
        body: { path: string | null };
      };
      const found = this.find(asked.params.path.id);
      if (!found) return this.reply(404, undefined, "no such workspace");
      this.list = this.list.map((w) =>
        w.id === found.id ? { ...w, start_folder: asked.body.path } : w,
      );
      return this.reply(200, {
        start_folder: asked.body.path,
        exists:
          found.status === "running" && asked.body.path !== null ? true : null,
      });
    }
    if (path === "/api/workspaces/{id}/session-user") {
      const asked = init as unknown as {
        params: { path: { id: string } };
        body: { session_user: Workspace["session_user"] };
      };
      const found = this.find(asked.params.path.id);
      if (!found) return this.reply(404, undefined, "no such workspace");
      const changed = { ...found, session_user: asked.body.session_user };
      this.list = this.list.map((w) => (w.id === found.id ? changed : w));
      return this.reply(200, changed);
    }
    const key = init.params.path.workspace;
    this.overrides[key] = init.body.overrides;
    this.list = this.list.map((w) =>
      w.id === key
        ? { ...w, direct_ssh: init.body.overrides.direct_ssh === true }
        : w,
    );
    return this.reply(200, {
      workspace: key,
      overrides: this.overrides[key],
      effective: this.effective(key),
      unknown_fields: [],
    });
  };
}
