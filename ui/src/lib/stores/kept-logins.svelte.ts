// SPDX-License-Identifier: GPL-3.0-or-later
// The logins Puddle keeps for one workspace, for its settings page: which services, since when.
// The list never holds a token; forgetting one asks the API to delete it from the credential store.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { forgetKeptLogin } from "#lib/notify/logins.ts";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

export type KeptLogin = components["schemas"]["KeptLoginView"];
type StoreApi = Pick<ApiClient, "GET" | "DELETE">;

export type Status = "loading" | "ready" | "failed";
export type ForgetResult = { ok: true } | { ok: false; message: string };

export class KeptLogins {
  status = $state<Status>("loading");
  logins = $state.raw<KeptLogin[]>([]);
  /** False when the credential store did not answer and Puddle had no record of its own. */
  storeReadable = $state(true);
  /** Why the list can't be read, when it can't. */
  reason = $state<string | null>(null);
  #id = "";
  /** Calls to `load` in order, so a slow answer never overwrites a newer one. */
  #ticket = 0;
  readonly #api: StoreApi;

  constructor(api: StoreApi = defaultApi) {
    this.#api = api;
  }

  /** Reads the list. `quiet` keeps what is on screen while it does, and when it fails. */
  async load(id: string, quiet = false): Promise<void> {
    if (id !== this.#id) quiet = false;
    this.#id = id;
    if (!quiet) {
      this.status = "loading";
      this.reason = null;
    }
    const ticket = ++this.#ticket;
    try {
      const { data, error, response } = await this.#api.GET(
        "/api/workspaces/{id}/logins",
        { params: { path: { id } } },
      );
      if (ticket !== this.#ticket) return;
      if (data) {
        this.logins = data.logins;
        this.storeReadable = data.store_readable;
        this.reason = null;
        this.status = "ready";
      } else if (!quiet) {
        this.status = "failed";
        this.reason = readFailure(error, response);
      }
    } catch {
      if (ticket === this.#ticket && !quiet) {
        this.status = "failed";
        this.reason = SERVICE_DOWN;
      }
    }
  }

  /** Forgets one service's login; the row leaves the list only once the API says it is gone. */
  async forget(service: string): Promise<ForgetResult> {
    try {
      await forgetKeptLogin(this.#api, this.#id, service);
    } catch (error) {
      return {
        ok: false,
        message: error instanceof Error ? error.message : String(error),
      };
    }
    // A read that began before the deletion must not bring the row back.
    this.#ticket++;
    this.logins = this.logins.filter((login) => login.service !== service);
    return { ok: true };
  }
}
