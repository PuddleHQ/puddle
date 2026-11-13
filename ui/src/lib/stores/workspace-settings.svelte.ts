// SPDX-License-Identifier: AGPL-3.0-or-later
// One workspace's settings: its overrides, what is in effect, and the global values the
// "use the global setting" choices name. A change is saved at once and replaces the overrides
// (the API replaces, so every save sends the whole layer).
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { unreadableReason } from "#lib/settings/model.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";
import type { Layer } from "#lib/workspaces/settings.ts";

type Effective = components["schemas"]["EffectiveSettings"];
export type Tools = components["schemas"]["WorkspaceTools"];
type StoreApi = Pick<ApiClient, "GET" | "PUT">;

export type Status = "loading" | "ready" | "failed";
export type SaveResult = { ok: true } | { ok: false; message: string };

export class WorkspaceSettings {
  status = $state<Status>("loading");
  overrides = $state.raw<Layer | null>(null);
  effective = $state.raw<Effective | null>(null);
  /** This workspace's own tool switches; `null` is Puddle's default (on). */
  tools = $state.raw<Tools>({ mise: null, repo_code: null });
  /** The global mise switch: while it is off the workspace's switches are hidden and do nothing. */
  globalMise = $state(true);
  /** Fields in the stored document that this Puddle doesn't know. */
  unknownFields = $state.raw<string[]>([]);
  /** What a workspace with no overrides gets: the global values. */
  global = $state.raw<Effective | null>(null);
  /** Why the settings can't be read, when the API says (a damaged settings file). */
  reason = $state<string | null>(null);
  #id = "";
  /** Calls to `load` and `change` in order, so a slow answer never overwrites a newer one. */
  #ticket = 0;
  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  /** Reads both layers. `quiet` keeps the form on screen while it does (after a failed save). */
  async load(id: string, quiet = false): Promise<void> {
    this.#id = id;
    if (!quiet) {
      this.status = "loading";
      this.reason = null;
    }
    const ticket = ++this.#ticket;
    try {
      const [mine, global] = await Promise.all([
        this.#api.GET("/api/settings/workspaces/{workspace}", {
          params: { path: { workspace: id } },
        }),
        this.#api.GET("/api/settings"),
      ]);
      if (ticket !== this.#ticket) return;
      if (mine.data && global.data) {
        this.overrides = mine.data.overrides;
        this.tools = mine.data.tools;
        this.globalMise = global.data.tools.mise ?? true;
        this.effective = mine.data.effective;
        this.unknownFields = mine.data.unknown_fields;
        this.global = global.data.effective;
        this.status = "ready";
      } else if (!quiet) {
        this.status = "failed";
        this.reason =
          unreadableReason(mine.error, global.error) ??
          (mine.data
            ? readFailure(global.error, global.response)
            : readFailure(mine.error, mine.response));
      }
    } catch {
      if (ticket === this.#ticket && !quiet) {
        this.status = "failed";
        this.reason = SERVICE_DOWN;
      }
    }
  }

  /** Saves one override (or several); `null` makes a setting inherit again. */
  change(patch: Partial<Layer>): Promise<SaveResult> {
    return this.#put((overrides) => ({ ...overrides, ...patch }), {});
  }

  /** Saves the tool switches; `null` is Puddle's default again. */
  changeTools(patch: Partial<Tools>): Promise<SaveResult> {
    return this.#put((overrides) => overrides, patch);
  }

  /** The API replaces the whole document, so each save sends both parts as stored with the change laid over. */
  async #put(
    layer: (overrides: Layer) => Layer,
    tools: Partial<Tools>,
  ): Promise<SaveResult> {
    if (!this.overrides)
      return { ok: false, message: "Settings aren't loaded." };
    const ticket = ++this.#ticket;
    try {
      const { data, error } = await this.#api.PUT(
        "/api/settings/workspaces/{workspace}",
        {
          params: { path: { workspace: this.#id } },
          body: {
            overrides: layer(this.overrides),
            tools: { ...this.tools, ...tools },
          },
        },
      );
      if (!data) {
        return {
          ok: false,
          message: error?.message ?? "Puddle couldn't save that.",
        };
      }
      if (ticket === this.#ticket) {
        this.overrides = data.overrides;
        this.tools = data.tools;
        this.effective = data.effective;
      }
      return { ok: true };
    } catch {
      return { ok: false, message: "Puddle's service isn't answering." };
    }
  }
}
