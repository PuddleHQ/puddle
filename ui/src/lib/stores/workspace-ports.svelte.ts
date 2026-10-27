// SPDX-License-Identifier: GPL-3.0-or-later
// One workspace's ports for its tab: what listens in it, what is forwarded to this computer and
// what is pinned. The page reads the list again on the events that say it changed and on a resync.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import { sentence } from "#lib/rules/model.ts";
import { isPortsEvent, type PortEntry } from "#lib/workspaces/ports.ts";
import type { LiveSource } from "./live.svelte.ts";
import { live } from "./live.svelte.ts";
import type { Result } from "./identities.svelte.ts";

export type Status = "loading" | "ready" | "failed";
type StoreApi = Pick<ApiClient, "GET" | "PUT" | "DELETE">;

const DOWN = "Puddle's service isn't answering.";

export class WorkspacePortsStore {
  ports = $state.raw<PortEntry[]>([]);
  /** The workspace runs, so a port can be forwarded. */
  running = $state(false);
  status = $state<Status>("loading");
  #name = "";
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
  async load(name: string, quiet = false): Promise<void> {
    this.#name = name;
    if (!quiet) this.status = "loading";
    const ticket = ++this.#ticket;
    try {
      const { data } = await this.#api.GET("/api/workspaces/{id}/ports", {
        params: { path: { id: name } },
      });
      if (ticket !== this.#ticket) return;
      if (data) {
        this.ports = data.ports;
        this.running = data.running;
        this.status = "ready";
      } else if (!quiet) {
        this.status = "failed";
      }
    } catch {
      if (ticket === this.#ticket && !quiet) this.status = "failed";
    }
  }

  /** Reads `name` now and keeps it current; returns the function that stops listening. */
  start(name: string): () => void {
    void this.load(name);
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if (isPortsEvent(event, name)) void this.load(name, true);
      },
      resync: () => void this.load(name, true),
    });
    return () => unsubscribe?.();
  }

  /**
   * Forwards `guest` to `host` on this computer (the same number when left out). `pinned` keeps the
   * forward across restarts of the workspace; asking again changes only that.
   */
  async forward(
    guest: number,
    host: number | null,
    pinned: boolean,
  ): Promise<Result> {
    try {
      const { data, error } = await this.#api.PUT(
        "/api/workspaces/{id}/ports/{port}",
        {
          params: { path: { id: this.#name, port: guest } },
          body: { host_port: host, pinned },
        },
      );
      if (!data) {
        return {
          ok: false,
          message: sentence(error?.message ?? "Puddle refused the forward"),
        };
      }
      await this.load(this.#name, true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  /** Stops the forward of `guest` and unpins it. One that is already gone counts as stopped. */
  async release(guest: number): Promise<Result> {
    try {
      const { response } = await this.#api.DELETE(
        "/api/workspaces/{id}/ports/{port}",
        { params: { path: { id: this.#name, port: guest } } },
      );
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: "Puddle couldn't stop the forward." };
      }
      await this.load(this.#name, true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }
}
