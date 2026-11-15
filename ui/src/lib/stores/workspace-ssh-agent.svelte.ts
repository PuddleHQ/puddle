// SPDX-License-Identifier: AGPL-3.0-or-later
// One workspace's SSH agent switch for its tab: the hosts the workspace may use the user's SSH
// agent for, with the keys each may use. Off for every host until the user turns it on for one.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import type { LiveSource } from "./live.svelte.ts";
import { live } from "./live.svelte.ts";
import type { Result } from "./identities.svelte.ts";

export type SshAgentHost = components["schemas"]["SshAgentHostView"];
export type Status = "loading" | "ready" | "failed";

type StoreApi = Pick<ApiClient, "GET" | "PUT" | "DELETE">;

const DOWN = "Puddle's service isn't answering.";

export class WorkspaceSshAgentStore {
  hosts = $state.raw<SshAgentHost[]>([]);
  status = $state<Status>("loading");
  #id = "";
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

  /** Reads the hosts. `quiet` keeps the page as it is while it does. */
  async load(id: string, quiet = false): Promise<void> {
    this.#id = id;
    if (!quiet) this.status = "loading";
    const ticket = ++this.#ticket;
    try {
      const { data } = await this.#api.GET("/api/workspaces/{id}/ssh-agent", {
        params: { path: { id } },
      });
      if (ticket !== this.#ticket) return;
      if (data) {
        this.hosts = data.hosts;
        this.status = "ready";
      } else if (!quiet) {
        this.status = "failed";
      }
    } catch {
      if (ticket === this.#ticket && !quiet) this.status = "failed";
    }
  }

  /** Reads workspace `id` now and keeps it current; returns the function that stops listening. */
  start(id: string): () => void {
    void this.load(id);
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if (
          typeof event === "object" &&
          event !== null &&
          (event as { type?: unknown; workspace?: unknown }).type ===
            "workspace_git_changed" &&
          (event as { workspace?: unknown }).workspace === id
        ) {
          void this.load(id, true);
        }
      },
      resync: () => void this.load(id, true),
    });
    return () => unsubscribe?.();
  }

  /** Turns the agent on for `host`, for every key (`null`) or only the listed fingerprints. */
  async turnOn(host: string, keys: string[] | null): Promise<Result> {
    try {
      const { data, error } = await this.#api.PUT(
        "/api/workspaces/{id}/ssh-agent/{host}",
        {
          params: { path: { id: this.#id, host } },
          body: keys === null ? {} : { keys },
        },
      );
      if (!data) {
        return {
          ok: false,
          message: error?.message ?? "Puddle refused the host.",
        };
      }
      await this.load(this.#id, true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  /** Turns the agent off for `host`. A host it was not on for counts as off. */
  async turnOff(host: string): Promise<Result> {
    try {
      const { response } = await this.#api.DELETE(
        "/api/workspaces/{id}/ssh-agent/{host}",
        { params: { path: { id: this.#id, host } } },
      );
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: "Puddle couldn't turn the agent off." };
      }
      await this.load(this.#id, true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }
}
