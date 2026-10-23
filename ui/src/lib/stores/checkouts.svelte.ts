// SPDX-License-Identifier: GPL-3.0-or-later
// One workspace's checkouts for its Repositories section: the listing of what is on the disk next
// to the list's entries, and the four calls that change the list (add a clone, manage a checkout
// that is already there, clone an entry again, remove an entry). The listing is read again when
// the workspace's list changes, when the workspace starts, stops or finishes an operation, and on
// a resync. Every string in a listing came from the guest.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { sentence } from "#lib/rules/model.ts";
import type { CheckoutListing } from "#lib/workspaces/checkouts.ts";
import type { LiveSource } from "./live.svelte.ts";
import { live } from "./live.svelte.ts";
import type { Result } from "./identities.svelte.ts";
import { workspaces } from "./workspaces.svelte.ts";

export type AddRepository = components["schemas"]["AddRepositoryRequest"];
export type Status = "loading" | "ready" | "failed";

type StoreApi = Pick<ApiClient, "GET" | "POST" | "DELETE">;

const DOWN = "Puddle's service isn't answering.";
const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null;

/** Whether `event` can change what the section shows for workspace `name`. */
export function touchesCheckouts(event: unknown, name: string): boolean {
  if (!isObject(event) || event["workspace"] !== name) return false;
  switch (event["type"]) {
    case "workspace_repositories_changed":
    case "status_changed":
      return true;
    case "workspace_progress":
      return event["step"] === "done" || event["step"] === "failed";
    default:
      return false;
  }
}

export class CheckoutsStore {
  listing = $state.raw<CheckoutListing | null>(null);
  status = $state<Status>("loading");
  /** Why the last read failed, when it did. */
  error = $state<string | null>(null);
  #name = "";
  /** Calls to `load` in order, so a slow answer never overwrites a newer one. */
  #ticket = 0;
  readonly #api: StoreApi;
  readonly #source: LiveSource | undefined;
  readonly #refreshWorkspaces: () => Promise<void>;

  constructor(
    api: StoreApi = defaultApi,
    source: LiveSource | undefined = live,
    refreshWorkspaces: () => Promise<void> = () => workspaces.refresh(),
  ) {
    this.#api = api;
    this.#source = source;
    this.#refreshWorkspaces = refreshWorkspaces;
  }

  /** Reads the listing. `quiet` keeps what the page shows while it does. */
  async load(name: string, quiet = false): Promise<void> {
    if (name !== this.#name) {
      this.#name = name;
      this.listing = null;
    }
    if (!quiet) this.status = "loading";
    const ticket = ++this.#ticket;
    try {
      const { data, error } = await this.#api.GET(
        "/api/workspaces/{id}/checkouts",
        { params: { path: { id: name } } },
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.listing = data;
        this.status = "ready";
        this.error = null;
      } else {
        this.status = this.listing ? "ready" : "failed";
        this.error = sentence(error?.message ?? "Puddle refused to list them");
      }
    } catch {
      if (ticket !== this.#ticket) return;
      this.status = this.listing ? "ready" : "failed";
      this.error = DOWN;
    }
  }

  /** Reads `name` now and keeps it current; returns the function that stops listening. */
  start(name: string): () => void {
    void this.load(name);
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if (touchesCheckouts(event, name)) void this.load(name, true);
      },
      resync: () => void this.load(name, true),
    });
    return () => unsubscribe?.();
  }

  /** After a change the workspace's record (its list, its busy state) and the listing are read again. */
  async #changed(): Promise<void> {
    await this.#refreshWorkspaces();
    await this.load(this.#name, true);
  }

  async #send(
    run: () => Promise<{
      data?: unknown;
      error?: { message?: string };
    }>,
    fallback: string,
  ): Promise<Result> {
    try {
      const { data, error } = await run();
      if (data === undefined) {
        return { ok: false, message: sentence(error?.message ?? fallback) };
      }
      await this.#changed();
      return { ok: true };
    } catch {
      return { ok: false, message: DOWN };
    }
  }

  /**
   * Adds a repository to the workspace: a clone (the workspace is busy cloning until it ends) or
   * a checkout that is already there. A refusal comes back as text, with 409 naming both
   * addresses when the checkout's remote is no longer the one the user saw.
   */
  add(body: AddRepository): Promise<Result> {
    return this.#send(
      () =>
        this.#api.POST("/api/workspaces/{id}/repositories", {
          params: { path: { id: this.#name } },
          body,
        }),
      "Puddle refused to add the repository",
    );
  }

  /** Clones an entry whose directory is not on the disk again. */
  cloneAgain(directory: string): Promise<Result> {
    return this.#send(
      () =>
        this.#api.POST("/api/workspaces/{id}/repositories/{directory}/clone", {
          params: { path: { id: this.#name, directory } },
        }),
      "Puddle refused to clone it again",
    );
  }

  /** Takes an entry off the list. The folder and its files stay. */
  remove(directory: string): Promise<Result> {
    return this.#send(
      () =>
        this.#api.DELETE("/api/workspaces/{id}/repositories/{directory}", {
          params: { path: { id: this.#name, directory } },
        }),
      "Puddle refused to remove it from the list",
    );
  }
}
