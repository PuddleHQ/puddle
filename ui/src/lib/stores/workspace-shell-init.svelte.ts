// SPDX-License-Identifier: GPL-3.0-or-later
// One workspace's shell init for its tab: the saved script, a save that is refused with what the
// script printed (nothing is saved then), and the notice that something inside the workspace
// changed Puddle's copy and Puddle put it back. The page reads the script again on
// `workspace_shell_init_changed` and on a resync.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { shellInitEvent } from "#lib/workspaces/shell-init.ts";
import type { LiveSource } from "./live.svelte.ts";
import { live } from "./live.svelte.ts";
import type { Result } from "./identities.svelte.ts";

export type ShellInit = components["schemas"]["ShellInitView"];
export type SavedShellInit = components["schemas"]["ShellInitSaved"];
export type Status = "loading" | "ready" | "failed";

type StoreApi = Pick<ApiClient, "GET" | "PUT" | "DELETE">;

const DOWN = "Puddle's service isn't answering.";

export class ShellInitStore {
  view = $state.raw<ShellInit | null>(null);
  status = $state<Status>("loading");
  /** Puddle put the saved text back after something in the workspace changed it. */
  restored = $state(false);
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

  /** Reads the saved script. `quiet` keeps the page as it is while it does. */
  async load(name: string, quiet = false): Promise<void> {
    this.#name = name;
    if (!quiet) this.status = "loading";
    const ticket = ++this.#ticket;
    try {
      const { data } = await this.#api.GET("/api/workspaces/{id}/shell-init", {
        params: { path: { id: name } },
      });
      if (ticket !== this.#ticket) return;
      if (data) {
        this.view = data;
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
    this.restored = false;
    void this.load(name);
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        const kind = shellInitEvent(event, name);
        if (kind === "changed") void this.load(name, true);
        if (kind === "restored") this.restored = true;
      },
      resync: () => void this.load(name, true),
    });
    return () => unsubscribe?.();
  }

  /**
   * Saves the script. A running workspace runs it once first, and a script that prints, fails or
   * runs too long is refused with what was seen, in `message`, as the service wrote it.
   */
  async save(content: string): Promise<Result<SavedShellInit>> {
    try {
      const { data, error } = await this.#api.PUT(
        "/api/workspaces/{id}/shell-init",
        { params: { path: { id: this.#name } }, body: { content } },
      );
      if (!data) {
        return {
          ok: false,
          message: error?.message ?? "Puddle refused the script.",
        };
      }
      this.view = data.shell_init;
      this.status = "ready";
      return { ok: true, value: data };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  /** Removes the saved script. One that is already gone counts as removed. */
  async remove(): Promise<Result> {
    try {
      const { response } = await this.#api.DELETE(
        "/api/workspaces/{id}/shell-init",
        { params: { path: { id: this.#name } } },
      );
      if (!response.ok && response.status !== 404) {
        return { ok: false, message: "Puddle couldn't remove the script." };
      }
      await this.load(this.#name, true);
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }
}
