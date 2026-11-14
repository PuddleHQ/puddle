// SPDX-License-Identifier: AGPL-3.0-or-later
// The credential store problem for the first-run banner: the host raises one problem with the key
// `credential-store` when the operating system's store is locked or missing, and "Retry" asks it
// to connect again and check, so unlocking the store needs no restart.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { live, type LiveSource } from "./live.svelte.ts";

export type CredentialStoreProblem = components["schemas"]["Problem"];
type StoreApi = Pick<ApiClient, "GET" | "POST">;

/** The key the host files the problem under. */
export const CREDENTIAL_STORE_KEY = "credential-store";

export class CredentialStoreState {
  /** The standing problem; `null` while the store works and until the first read. */
  problem = $state.raw<CredentialStoreProblem | null>(null);
  /** A retry is running. */
  retrying = $state(false);
  /** Why the last retry could not be asked, in words; `null` otherwise. */
  retryError = $state<string | null>(null);
  /** The last retry reached the store and it still refused. */
  stillBroken = $state(false);

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

  /** Reads the problem list. A failed read leaves what is shown as it is. */
  async load(): Promise<void> {
    const ticket = ++this.#ticket;
    try {
      const { data } = await this.#api.GET("/api/problems");
      if (ticket !== this.#ticket || !data) return;
      this.problem =
        data.problems.find((p) => p.key === CREDENTIAL_STORE_KEY) ?? null;
    } catch {
      // The banner stays as it was.
    }
  }

  /**
   * Asks the host to connect to the store again and check it. The banner ends when the host
   * clears the problem; a store that still refuses sets `stillBroken`. One request at a time;
   * never throws.
   */
  async retry(): Promise<void> {
    if (this.retrying) return;
    this.retrying = true;
    this.retryError = null;
    this.stillBroken = false;
    try {
      const { data, error } = await this.#api.POST(
        "/api/credential-store/retry",
      );
      if (data) {
        this.stillBroken = !data.working;
      } else {
        this.retryError = error?.message ?? "Puddle refused that.";
      }
    } catch {
      this.retryError = "Puddle could not be reached.";
    } finally {
      this.retrying = false;
    }
    await this.load();
  }

  /** Reads now and follows the host's changes; returns the function that stops listening. */
  start(): () => void {
    void this.load();
    const unsubscribe = this.#source?.subscribe({
      event: (event) => {
        if ((event as { type?: unknown } | null)?.type === "problems_changed")
          void this.load();
      },
      resync: () => void this.load(),
    });
    return () => unsubscribe?.();
  }
}

export const credentialStore = new CredentialStoreState();
