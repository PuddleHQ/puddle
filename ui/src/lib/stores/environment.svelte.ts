// SPDX-License-Identifier: GPL-3.0-or-later
// The environment variables and secrets of one scope: a workspace's own (with the global ones it
// inherits, listed after them) or the global ones. A secret's value goes out once and never comes
// back; the table has only its hosts. Every change is saved at once, and the list is read again on
// the events that say it changed (and on a resync).
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { EnvSet, EnvVariable } from "#lib/environment/model.ts";
import { isEnvChanged } from "#lib/environment/events.ts";
import { sentence } from "#lib/rules/model.ts";
import type { LiveSource } from "./live.svelte.ts";
import { live } from "./live.svelte.ts";
import type { Result } from "./identities.svelte.ts";

type StoreApi = Pick<ApiClient, "GET" | "PUT" | "DELETE">;
export type Status = "loading" | "ready" | "failed";

/** The global variables, or those of the workspace with this name. */
export type EnvScopeKey =
  { kind: "global" } | { kind: "workspace"; name: string };

const DOWN = "Puddle's service isn't answering.";

export class EnvironmentStore {
  variables = $state.raw<EnvVariable[]>([]);
  status = $state<Status>("loading");
  #scope: EnvScopeKey = { kind: "global" };
  /** Calls to `load` in order, so a slow answer never overwrites a newer one. */
  #ticket = 0;
  readonly #api: StoreApi;
  readonly #source: LiveSource | undefined;

  constructor(
    api: StoreApi = defaultApi,
    source: LiveSource | undefined = live,
  ) {
    this.#api = api;
    this.#source = source;
  }

  /** Reads the list. `quiet` keeps the page as it is while it does. */
  async load(scope: EnvScopeKey, quiet = false): Promise<void> {
    this.#scope = scope;
    if (!quiet) this.status = "loading";
    const ticket = ++this.#ticket;
    try {
      const data =
        scope.kind === "global"
          ? (await this.#api.GET("/api/env")).data
          : (
              await this.#api.GET("/api/workspaces/{id}/env", {
                params: { path: { id: scope.name } },
              })
            ).data;
      if (ticket !== this.#ticket) return;
      if (data) {
        this.variables = data.variables;
        this.status = "ready";
      } else if (!quiet) {
        this.status = "failed";
      }
    } catch {
      if (ticket === this.#ticket && !quiet) this.status = "failed";
    }
  }

  /** Reads `scope` now and keeps it current; returns the function that stops listening. */
  start(scope: EnvScopeKey): () => void {
    void this.load(scope);
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if (isEnvChanged(event, scope)) void this.load(scope, true);
      },
      resync: () => void this.load(scope, true),
    });
    return () => unsubscribe?.();
  }

  /** Sets a variable or a secret. Leaving a secret's `value` out keeps the one it has. */
  async set(name: string, body: EnvSet): Promise<Result> {
    const scope = this.#scope;
    try {
      const { data, error } =
        scope.kind === "global"
          ? await this.#api.PUT("/api/env/{name}", {
              params: { path: { name } },
              body,
            })
          : await this.#api.PUT("/api/workspaces/{id}/env/{name}", {
              params: { path: { id: scope.name, name } },
              body,
            });
      if (!data) {
        return {
          ok: false,
          message: sentence(error?.message ?? "Puddle refused the variable"),
        };
      }
      await this.load(scope, true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  /** Removes a variable or secret. One that is already gone counts as removed. */
  async remove(name: string): Promise<Result> {
    const scope = this.#scope;
    try {
      const { response } =
        scope.kind === "global"
          ? await this.#api.DELETE("/api/env/{name}", {
              params: { path: { name } },
            })
          : await this.#api.DELETE("/api/workspaces/{id}/env/{name}", {
              params: { path: { id: scope.name, name } },
            });
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: "Puddle couldn't remove the variable." };
      }
      await this.load(scope, true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }
}
